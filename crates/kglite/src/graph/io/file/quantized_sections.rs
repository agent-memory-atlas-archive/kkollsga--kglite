//! Quantized scan copies in a v9 container.
//!
//! The *declaration* of a copy — which store, which precision, how much
//! oversampling — travels as `quantization` records in the file metadata, so
//! the postcard store record is untouched and an older reader that does not
//! know the field still loads the canonical `f32` store. The copy itself is
//! derived data written as two optional sections beside the store's vectors:
//! `emb:<type>:<property>:qcodes` (dtype `f16`/`i8`/bit-packed) and, for
//! `int8`, `…:qscales` (one `f32` per vector). A store that does not go to the
//! data region (small, or `layout="compact"`) writes the record only, and the
//! load derives the copy again; a copy that fails its shape check is
//! re-derived the same way, never refused.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::io;

use serde::{Deserialize, Serialize};

use super::embedding_sections::{section_name, EDGE_SCOPE, NODE_SCOPE};
use super::invalid_data;
use super::mappable::{MappableSection, MappableTable, MappedSection, SectionDtype};
use crate::graph::quantized::{CodeBuf, Codes, Precision, QuantStore};
use crate::graph::schema::{DirGraph, EmbeddingStore};
use crate::graph::vector_buf::VectorBuf;

/// Section classes owned by this module.
pub(super) const KIND_QCODES: u8 = 4;
pub(super) const KIND_QSCALES: u8 = 5;

const NODE: &str = "node";
const EDGE: &str = "edge";

/// One declared quantized copy, as the file metadata records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct QuantRecord {
    /// `"node"` or `"edge"`.
    pub(crate) scope: String,
    pub(crate) first: String,
    pub(crate) second: String,
    pub(crate) precision: String,
    pub(crate) oversample: u32,
}

/// The records a save of `graph` writes: every store holding a copy, node
/// stores then relationship stores, each by key.
pub(super) fn collect_records(graph: &DirGraph) -> Vec<QuantRecord> {
    let mut out = Vec::new();
    let node: BTreeMap<_, _> = graph.embeddings.iter().collect();
    for (key, store) in node {
        push_record(&mut out, NODE, (&key.0, &key.1), store);
    }
    let edge: BTreeMap<_, _> = graph.edge_embeddings.iter().collect();
    for (key, store) in edge {
        push_record(&mut out, EDGE, (&key.0, &key.1), store.numeric());
    }
    out
}

fn push_record(
    out: &mut Vec<QuantRecord>,
    scope: &str,
    keys: (&str, &str),
    store: &EmbeddingStore,
) {
    if let Some(quant) = store.quant_store() {
        out.push(QuantRecord {
            scope: scope.to_string(),
            first: keys.0.to_string(),
            second: keys.1.to_string(),
            precision: quant.precision().name().to_string(),
            oversample: quant.oversample(),
        });
    }
}

#[cfg(target_endian = "little")]
fn u16_bytes(values: &[u16]) -> Cow<'_, [u8]> {
    // SAFETY: any u16 slice is valid as bytes (no padding, align 1 <= 2); the
    // borrow keeps it alive and unchanged; little-endian hosts write the
    // format's byte order directly.
    Cow::Borrowed(unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    })
}

#[cfg(not(target_endian = "little"))]
fn u16_bytes(values: &[u16]) -> Cow<'_, [u8]> {
    Cow::Owned(values.iter().flat_map(|v| v.to_le_bytes()).collect())
}

fn i8_bytes(values: &[i8]) -> Cow<'_, [u8]> {
    // SAFETY: i8 and u8 have the same size, alignment and valid bit patterns.
    Cow::Borrowed(unsafe { std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), values.len()) })
}

#[cfg(target_endian = "little")]
fn f32_bytes(values: &[f32]) -> Cow<'_, [u8]> {
    // SAFETY: as in `embedding_sections::f32_bytes`.
    Cow::Borrowed(unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    })
}

#[cfg(not(target_endian = "little"))]
fn f32_bytes(values: &[f32]) -> Cow<'_, [u8]> {
    Cow::Owned(values.iter().flat_map(|v| v.to_le_bytes()).collect())
}

/// The sections of `store`'s quantized copy (none when it holds no usable
/// copy). Called only for a store whose vectors go to the data region.
pub(super) fn push_sections<'g>(
    out: &mut Vec<MappableSection<'g>>,
    scope: &str,
    owner: &str,
    keys: (&str, &str),
    store: &'g EmbeddingStore,
) {
    let Some(quant) = store.quantized() else {
        return;
    };
    let (dtype, elem_count, data) = match quant.codes() {
        Codes::F16(buf) => (
            SectionDtype::F16,
            buf.as_slice().len(),
            u16_bytes(buf.as_slice()),
        ),
        Codes::Int8(buf) => (
            SectionDtype::I8,
            buf.as_slice().len(),
            i8_bytes(buf.as_slice()),
        ),
        Codes::Bits(buf) => (
            SectionDtype::BitPacked,
            buf.as_slice().len(),
            Cow::Borrowed(buf.as_slice()),
        ),
    };
    out.push(MappableSection {
        name: section_name(scope, keys.0, keys.1, "qcodes"),
        owner: owner.to_string(),
        kind: KIND_QCODES,
        dtype,
        elem_count: elem_count as u64,
        flags: 0,
        data,
    });
    if quant.precision() == Precision::Int8 {
        out.push(MappableSection {
            name: section_name(scope, keys.0, keys.1, "qscales"),
            owner: owner.to_string(),
            kind: KIND_QSCALES,
            dtype: SectionDtype::F32,
            elem_count: quant.scales().len() as u64,
            flags: 0,
            data: f32_bytes(quant.scales()),
        });
    }
}

fn heap_codes_u16(section: &MappedSection) -> Vec<u16> {
    section
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect()
}

fn scales_buffer(section: &MappedSection) -> VectorBuf {
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

fn codes_of(section: &MappedSection) -> io::Result<Codes> {
    Ok(match section.dtype {
        SectionDtype::F16 => Codes::F16(
            CodeBuf::<u16>::mapped(section.clone())
                .unwrap_or_else(|| CodeBuf::Heap(heap_codes_u16(section))),
        ),
        SectionDtype::I8 => {
            Codes::Int8(CodeBuf::<i8>::mapped(section.clone()).unwrap_or_else(|| {
                CodeBuf::Heap(section.as_bytes().iter().map(|&b| b as i8).collect())
            }))
        }
        SectionDtype::BitPacked => Codes::Bits(
            CodeBuf::<u8>::mapped(section.clone())
                .unwrap_or_else(|| CodeBuf::Heap(section.as_bytes().to_vec())),
        ),
        _ => {
            return Err(invalid_data(format!(
                "mappable section '{}' is not a quantized code section",
                section.name
            )))
        }
    })
}

/// Give `store` its quantized copy: the persisted sections when the table has
/// them and they fit the store, else a copy derived from the `f32` vectors
/// when the file declares one. A table entry with no declaration is refused.
pub(super) fn attach(
    table: &MappableTable,
    claimed: &mut HashSet<String>,
    scope: &str,
    keys: (&str, &str),
    store: &mut EmbeddingStore,
    records: &[QuantRecord],
) -> io::Result<()> {
    let tag = if scope == EDGE_SCOPE { EDGE } else { NODE };
    debug_assert!(scope == EDGE_SCOPE || scope == NODE_SCOPE);
    let record = records
        .iter()
        .find(|r| r.scope == tag && r.first == keys.0 && r.second == keys.1);
    let codes_name = section_name(scope, keys.0, keys.1, "qcodes");
    let scales_name = section_name(scope, keys.0, keys.1, "qscales");
    let codes_section = table.get(&codes_name);
    let Some(record) = record else {
        return match codes_section {
            Some(_) => Err(invalid_data(format!(
                "mappable section '{codes_name}' has no quantization record in the file metadata"
            ))),
            None => Ok(()),
        };
    };
    let precision = Precision::parse(&record.precision).map_err(invalid_data)?;
    let attached = codes_section.and_then(|section| {
        if section.kind != KIND_QCODES {
            return None;
        }
        let codes = codes_of(section).ok()?;
        let scales = match table.get(&scales_name) {
            Some(s) if s.kind == KIND_QSCALES && s.dtype == SectionDtype::F32 => scales_buffer(s),
            _ => VectorBuf::new(),
        };
        QuantStore::from_parts(
            precision,
            record.oversample,
            store.dimension,
            store.slot_to_node.len(),
            codes,
            scales,
        )
    });
    claimed.insert(codes_name);
    claimed.insert(scales_name);
    match attached {
        Some(quant) => store.attach_quant(quant),
        // Derived data: an absent or ill-fitting copy is rebuilt, and a
        // store that cannot be quantized (shape errors surface in the
        // validation that follows) simply loads without one.
        None => {
            let _ = store.quantize(precision, Some(record.oversample));
        }
    }
    Ok(())
}
