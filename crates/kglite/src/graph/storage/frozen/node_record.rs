//! The node array of a frozen topology: `NodeData` records read in place.
//!
//! A frozen `.kgl` stores one 32-byte record per node slot, laid out exactly
//! as the compiler lays out a `NodeData` whose `ident` is `None` and whose
//! `properties` are `Columnar`:
//!
//! ```text
//!  0.. 8  node_type   u64 (InternedKey)
//!  8..16  ident       0   (Option<Box<..>> None, the null niche)
//! 16..17  tag         1   (PropertyStorage::Columnar)
//! 17..20  padding     0
//! 20..24  row_id      u32
//! 24..32  padding     0
//! ```
//!
//! `NodeData` is `repr(C)` and `PropertyStorage` is `repr(u8)`, so this layout
//! is a language guarantee rather than a rustc choice, and the const assertions
//! below pin the sizes and offsets the guarantee implies. A record in the file
//! is only ever bit-valid for that one shape, so a loader checks every record
//! (`validate`) before it hands out `&[NodeData]`: an invalid tag or a non-null
//! `ident` read as a `NodeData` would be undefined behaviour, and a file is
//! untrusted input.
//!
//! `NodeData` has drop glue (`ident` owns a box), but a record that passed
//! `validate` has `ident == None`, so dropping one frees nothing; a mapped
//! slab never drops its records, and an owned slab drops `Vec<NodeData>` of
//! ordinary values.

use std::io;
use std::sync::Arc;

use crate::graph::io::file::mappable::{MappedSection, SharedSection};
use crate::graph::schema::NodeData;
use crate::graph::storage::interner::InternedKey;
use crate::graph::storage::property_storage::{ColumnarRow, PropertyStorage};

pub(crate) const RECORD_BYTES: usize = 32;
const TAG_OFFSET: usize = 16;
const ROW_OFFSET: usize = 20;
const COLUMNAR_TAG: u8 = 1;

const _: () = {
    assert!(std::mem::size_of::<NodeData>() == RECORD_BYTES);
    assert!(std::mem::align_of::<NodeData>() == 8);
    assert!(std::mem::offset_of!(NodeData, node_type) == 0);
    assert!(std::mem::offset_of!(NodeData, ident) == 8);
    assert!(std::mem::offset_of!(NodeData, properties) == 16);
    assert!(std::mem::size_of::<PropertyStorage>() == 16);
    assert!(
        std::mem::size_of::<Option<Box<(crate::datatypes::Value, crate::datatypes::Value)>>>() == 8
    );
};

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("invalid frozen node record: {}", message.into()),
    )
}

/// The file bytes of the record for a live node of `node_type` on `row_id`.
pub(crate) fn encode_record(out: &mut Vec<u8>, node_type: InternedKey, row_id: u32) {
    out.extend_from_slice(&node_type.as_u64().to_le_bytes());
    out.extend_from_slice(&[0u8; 8]);
    out.push(COLUMNAR_TAG);
    out.extend_from_slice(&[0u8; 3]);
    out.extend_from_slice(&row_id.to_le_bytes());
    out.extend_from_slice(&[0u8; 8]);
}

/// The record a vacant slot holds: a valid `NodeData` shape that nothing reads,
/// because the alive bitset answers first.
pub(crate) fn encode_vacant(out: &mut Vec<u8>) {
    encode_record(out, InternedKey::from_u64(0), 0);
}

/// Decode one validated record into an ordinary `NodeData`.
fn decode_record(record: &[u8]) -> NodeData {
    let node_type = u64::from_le_bytes(record[0..8].try_into().expect("8 bytes"));
    let row_id = u32::from_le_bytes(
        record[ROW_OFFSET..ROW_OFFSET + 4]
            .try_into()
            .expect("4 bytes"),
    );
    NodeData {
        node_type: InternedKey::from_u64(node_type),
        ident: None,
        properties: PropertyStorage::Columnar(ColumnarRow::new(row_id)),
    }
}

/// Check that every 32-byte record is the `ident = None`, `Columnar` shape.
fn validate(bytes: &[u8]) -> io::Result<()> {
    if !bytes.len().is_multiple_of(RECORD_BYTES) {
        return Err(invalid("the section is not a whole number of records"));
    }
    for (index, record) in bytes.as_chunks::<RECORD_BYTES>().0.iter().enumerate() {
        if record[8..16] != [0u8; 8] || record[TAG_OFFSET] != COLUMNAR_TAG {
            return Err(invalid(format!(
                "record {index} is not a column-backed node without an inline identity"
            )));
        }
    }
    Ok(())
}

/// Where a slab's records live.
enum Backing {
    /// Keeps the mapping the slab's pointer addresses alive.
    Mapped(Arc<SharedSection>),
    Owned(Vec<NodeData>),
}

/// The node array: `NodeData` records in a mapped section, or owned values.
///
/// `ptr`/`len` mirror the slice `backing` serves, resolved once so a node read
/// is two loads with no variant test. The backing is never mutated, and a
/// `Vec`'s buffer does not move when the `Vec` does, so they stay valid for the
/// slab's life.
pub(crate) struct NodeSlab {
    backing: Backing,
    ptr: *const NodeData,
    len: usize,
}

// SAFETY: a mapped slab is a read-only view of a mapping its `Arc` keeps alive;
// an owned slab is a `Vec` of values that are `Send + Sync`. Nothing mutates the
// pointee.
unsafe impl Send for NodeSlab {}
// SAFETY: as above.
unsafe impl Sync for NodeSlab {}

impl NodeSlab {
    pub(crate) fn owned(nodes: Vec<NodeData>) -> Self {
        NodeSlab {
            ptr: nodes.as_ptr(),
            len: nodes.len(),
            backing: Backing::Owned(nodes),
        }
    }

    /// Validate `section` and serve its records: in place when it is mapped
    /// and 8-byte aligned, otherwise decoded into owned values.
    pub(crate) fn from_section(section: &MappedSection) -> io::Result<Self> {
        section.verify()?;
        let bytes = section.as_bytes();
        validate(bytes)?;
        let aligned = (bytes.as_ptr() as usize).is_multiple_of(std::mem::align_of::<NodeData>());
        if cfg!(target_endian = "little") && aligned && !bytes.is_empty() {
            if let Some(shared) = SharedSection::new(section.clone()) {
                return Ok(NodeSlab {
                    ptr: shared.bytes().as_ptr().cast::<NodeData>(),
                    len: shared.bytes().len() / RECORD_BYTES,
                    backing: Backing::Mapped(shared),
                });
            }
        }
        Ok(NodeSlab::owned(
            bytes
                .as_chunks::<RECORD_BYTES>()
                .0
                .iter()
                .map(|record| decode_record(record))
                .collect(),
        ))
    }

    #[inline(always)]
    pub(crate) fn as_slice(&self) -> &[NodeData] {
        // SAFETY: `ptr`/`len` describe `backing`'s records. For a mapped slab
        // `from_section` checked alignment and that every record is the
        // bit-valid `ident = None`, `Columnar` shape, `len` records lie inside
        // the section, and the `Arc` in `backing` keeps the read-only mapping
        // alive for as long as `self`. An owned slab is an ordinary `Vec`.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub(crate) fn shared_section(&self) -> Option<&Arc<SharedSection>> {
        match &self.backing {
            Backing::Mapped(section) => Some(section),
            Backing::Owned(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn heap_bytes(&self) -> usize {
        match &self.backing {
            Backing::Mapped(_) => 0,
            Backing::Owned(nodes) => nodes.len() * RECORD_BYTES,
        }
    }
}

impl Clone for NodeSlab {
    fn clone(&self) -> Self {
        match &self.backing {
            Backing::Mapped(section) => NodeSlab {
                backing: Backing::Mapped(Arc::clone(section)),
                ptr: self.ptr,
                len: self.len,
            },
            Backing::Owned(nodes) => NodeSlab::owned(nodes.clone()),
        }
    }
}

impl std::fmt::Debug for NodeSlab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "NodeSlab({} records, {})",
            self.as_slice().len(),
            if self.shared_section().is_some() {
                "mapped"
            } else {
                "owned"
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoder's bytes must be what the compiler lays out for a real
    /// `NodeData`, field by field: this is the check behind the in-place read.
    #[test]
    fn the_encoded_record_matches_the_compiler_layout_of_node_data() {
        let node = NodeData {
            node_type: InternedKey::from_str("Paper"),
            ident: None,
            properties: PropertyStorage::Columnar(ColumnarRow::new(0xA1B2_C3D4)),
        };
        let mut encoded = Vec::new();
        encode_record(&mut encoded, node.node_type, 0xA1B2_C3D4);
        assert_eq!(encoded.len(), RECORD_BYTES);
        let base = std::ptr::addr_of!(node).cast::<u8>();
        let read = |offset: usize, len: usize| -> Vec<u8> {
            (0..len)
                // SAFETY: `offset + i` stays inside `node`, and every byte read
                // belongs to an initialised field (never padding).
                .map(|i| unsafe { base.add(offset + i).read() })
                .collect()
        };
        assert_eq!(read(0, 8), encoded[0..8], "node_type");
        assert_eq!(read(8, 8), encoded[8..16], "ident is the null niche");
        assert_eq!(
            read(TAG_OFFSET, 1),
            encoded[TAG_OFFSET..TAG_OFFSET + 1],
            "tag"
        );
        assert_eq!(
            read(ROW_OFFSET, 4),
            encoded[ROW_OFFSET..ROW_OFFSET + 4],
            "row id"
        );
    }

    /// Reading the encoded bytes through the in-place path yields a value
    /// equal to the one a heap graph holds.
    #[test]
    fn an_encoded_record_read_in_place_equals_the_heap_node() {
        let mut bytes = Vec::new();
        encode_record(&mut bytes, InternedKey::from_str("Paper"), 7);
        encode_vacant(&mut bytes);
        validate(&bytes).unwrap();
        // 8-byte alignment for the cast, as a mapped section gives.
        let mut aligned = vec![0u64; bytes.len() / 8];
        // SAFETY: `aligned` is `bytes.len()` writable bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                aligned.as_mut_ptr().cast::<u8>(),
                bytes.len(),
            );
        }
        // SAFETY: both records validated above, `aligned` is 8-aligned and holds
        // two records.
        let nodes = unsafe { std::slice::from_raw_parts(aligned.as_ptr().cast::<NodeData>(), 2) };
        assert_eq!(nodes[0], decode_record(&bytes[0..32]));
        assert_eq!(nodes[0].node_type, InternedKey::from_str("Paper"));
        assert_eq!(nodes[0].properties.columnar_row_id(), Some(7));
        assert!(nodes[1].ident.is_none());
    }

    #[test]
    fn a_record_that_is_not_the_columnar_shape_is_refused() {
        let mut bytes = Vec::new();
        encode_record(&mut bytes, InternedKey::from_str("Paper"), 0);
        let mut bad_tag = bytes.clone();
        bad_tag[TAG_OFFSET] = 0;
        assert!(validate(&bad_tag).is_err(), "Map tag");
        bad_tag[TAG_OFFSET] = 2;
        assert!(validate(&bad_tag).is_err(), "unknown tag");
        let mut bad_ident = bytes.clone();
        bad_ident[8] = 1;
        assert!(validate(&bad_ident).is_err(), "non-null ident");
        assert!(validate(&bytes[..31]).is_err(), "partial record");
        assert!(validate(&bytes).is_ok());
    }
}
