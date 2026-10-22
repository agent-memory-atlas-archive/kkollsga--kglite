//! Mmap-resident `id_indices.bin` store with overlay for mutations.
//!
//! Replaces the eager `zstd::decode_all` + 124M-entry `HashMap::insert`
//! load path. Reads come from a memory-mapped flat binary on the disk;
//! mutations land in an in-memory overlay that takes precedence over
//! the base. On save, overlay + base are merged into a fresh `.bin`.
//!
//! ## File format `id_indices.bin`
//!
//! ```text
//! Header (32 bytes):
//!   [ 0.. 8]  magic           = b"KGLIIDXR"  (R = raw, mmap-friendly)
//!   [ 8..12]  version         = u32 LE (= 3; 2 is still read)
//!   [12..16]  num_types       = u32 LE
//!   [16..24]  dir_offset      = u64 LE   (always 32)
//!   [24..32]  data_offset     = u64 LE   (32 + 48 * num_types)
//!
//! Directory at [dir_offset]: 48 bytes per entry, sorted by type_key:
//!   [ 0.. 8]  type_key:    u64 LE  (InternedKey)
//!   [ 8.. 9]  variant:     u8      (0 = Integer, 1 = General, 2 = Int64Sorted)
//!   [ 9..16]  padding:     [u8; 7]
//!   [16..24]  num_entries: u64 LE
//!   [24..32]  payload_off: u64 LE   (file-relative)
//!   [32..40]  payload_len: u64 LE
//!   [40..48]  padding:     u64
//!
//! Data section at [data_offset]:
//!   Integer (variant=0):
//!     [payload_off..payload_off + 4*num_entries]               keys: [u32 sorted asc]
//!     [payload_off + 4*num_entries..payload_off + payload_len] idxs: [u32]
//!   General (variant=1):
//!     Postcard of HashMap<Value, NodeIndex>, length = payload_len
//!   Int64Sorted (variant=2; written by version 3, unknown to version 2):
//!     [payload_off..payload_off + 8*num_entries]               keys: [i64 sorted asc, unique]
//!     [payload_off + 8*num_entries..payload_off + payload_len] idxs: [u32]   (12 B per id)
//! ```
//!
//! Lookup is `O(log n)` binary search on `keys` for the Integer and Int64Sorted
//! variants (cache-friendly, ~24 comparisons even at 13M entries) and a single
//! `HashMap` probe for the General variant (decoded at load, where each
//! General payload is decoded to validate it). The writer emits Int64Sorted for
//! every type whose ids are all `Int64` and would otherwise have been a General
//! `HashMap<Value, NodeIndex>` at 60-90 B per id on the heap; an Int64Sorted
//! entry is searched in the mapping and never decoded onto the heap. The first
//! write to such a type layers a small delta over it ([`TypeEntry::OverBase`])
//! instead of cloning it, and a save merges the sorted delta into the sorted
//! base while streaming the new file.

use super::le_bytes::{le_i64_binary_search, le_u32_binary_search, read_le_i64, read_le_u32};
use crate::datatypes::Value;
use crate::graph::io::file::mappable::MappedSection;
use crate::graph::schema::{
    heal_general_spellings, id_spellings, id_u32, InternedKey, StringInterner, TypeIdIndex,
};
use crate::graph::storage::disk::id_index_layer::TypeEntry;
use crate::graph::storage::disk::id_index_shared::SharedIdIndexes;
use crate::serde_codec;
use memmap2::Mmap;
use petgraph::graph::NodeIndex;
use rustc_hash::FxHashMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

const MAGIC: &[u8; 8] = b"KGLIIDXR";
const VERSION: u32 = 3;
/// The previous file version. Identical layout; version 3 is the one that may
/// carry directory variants a version-2 reader has no decoder for, so writing
/// it makes those readers refuse the file by version instead of misreading it.
const VERSION_2: u32 = 2;
/// Directory variant tags.
const VARIANT_INTEGER: u8 = 0;
const VARIANT_GENERAL: u8 = 1;
pub(crate) const VARIANT_INT64: u8 = 2;
/// Sorted string ids. Only a `.kgl` id section base carries it: the directory
/// file's loader refuses the tag, so the file format is unchanged.
pub(crate) const VARIANT_STRING: u8 = 3;
/// Bytes per id of an Int64Sorted payload: an `i64` key and a `u32` node.
const INT64_ENTRY_BYTES: usize = 12;
const HEADER_BYTES: usize = 32;
const DIR_ENTRY_BYTES: usize = 48;
const MAX_GENERAL_INDEX_DECODE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

fn invalid_index(message: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("invalid id_indices.bin: {message}"),
    )
}

// Test-only tally of full `id -> node` maps built from a mapped entry. A
// per-row lookup must never move this.
#[cfg(test)]
thread_local! {
    static FULL_MAPS_BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn full_maps_built() -> usize {
    FULL_MAPS_BUILT.with(|count| count.get())
}

/// Read-only view of the id indexes of a graph: the mapping of a disk graph's
/// `id_indices.bin`, or the per-type sections of a v9 `.kgl`.
pub struct IdIndexBase {
    source: IdSource,
    /// type_name -> directory entry. Built once at load (88k entries × ~50 bytes ≈ 4 MB).
    /// Strings owned to keep the API HashMap-compatible without lifetime gymnastics.
    dir: HashMap<String, BaseEntry>,
    /// Decoded General payloads, one spelling per id: a payload persisted
    /// through 0.18.1 may spell an id twice, and decoding heals it. Filled at
    /// load, which decodes every General entry to validate it; `general_map`
    /// decodes only on a miss.
    /// Integer variant never enters here — it's read directly from mmap.
    general_cache: RwLock<HashMap<String, Arc<FxHashMap<Value, NodeIndex>>>>,
}

/// Where an [`IdIndexBase`]'s payload bytes live.
enum IdSource {
    /// One mapped file holding every type's payload.
    File {
        mmap: Arc<Mmap>,
        /// The file the mapping is of.
        origin: PathBuf,
        /// The layout version the file declares.
        version: u32,
        /// Keeps the generation this file lies in from being pruned: a type the
        /// writer left alone keeps serving from this mapping after later saves.
        _pin: Option<Arc<super::generation::GenerationPin>>,
    },
    /// Integer, Int64Sorted and string entries only, each as a section of
    /// sorted keys (little-endian `u32` or `i64`, or the offsets-and-bytes
    /// layout of [`string_keys`]) and a section of the `u32` nodes parallel to
    /// them; [`BaseEntry::section`] indexes this list.
    Sections(Vec<(MappedSection, MappedSection)>),
}

/// One type's directory entry in `id_indices.bin`: where its payload lies.
#[derive(Clone, Copy, Debug)]
pub struct BaseEntry {
    variant: u8,
    num_entries: u32,
    payload_off: u64,
    payload_len: u64,
    /// Position in [`IdSource::Sections`]; unused for a file.
    section: u32,
}

impl std::fmt::Debug for IdIndexBase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdIndexBase")
            .field("types", &self.dir.len())
            .finish()
    }
}

impl IdIndexBase {
    /// The stored spelling only, no numeric normalisation: what
    /// [`TypeIdIndex::get_exact`] answers for the same entry.
    pub(crate) fn lookup_exact(&self, name: &str, id: &Value) -> Option<NodeIndex> {
        let entry = self.dir.get(name)?;
        if entry.variant == VARIANT_INT64 {
            return self.lookup_int64_exact_at(entry, id);
        }
        if entry.variant == VARIANT_STRING {
            return self.lookup_string_at(entry, id);
        }
        if entry.variant == VARIANT_INTEGER {
            let Value::UniqueId(wanted) = id else {
                return None;
            };
            let (keys, nodes) = self.integer_bytes(entry)?;
            let position = le_u32_binary_search(keys, *wanted)?;
            return Some(NodeIndex::new(read_le_u32(nodes, position)? as usize));
        }
        self.general_map(name, entry)?.get(id).copied()
    }

    /// Ids indexed for `name`: a `General` entry's persisted count can
    /// exceed it by the spellings healed at load.
    pub(crate) fn entry_len(&self, name: &str) -> Option<usize> {
        let entry = self.dir.get(name)?;
        if entry.variant == VARIANT_GENERAL {
            return Some(self.general_map(name, entry)?.len());
        }
        Some(entry.num_entries as usize)
    }

    /// `name`'s directory entry when it is an Int64Sorted one.
    pub(crate) fn int64_entry(&self, name: &str) -> Option<BaseEntry> {
        self.dir
            .get(name)
            .filter(|entry| entry.variant == VARIANT_INT64)
            .copied()
    }

    /// `name`'s directory entry when it is searched in the mapping (Int64Sorted
    /// or string): what a delta layered over the mapping keeps, so a probe does
    /// not look the name up.
    pub(crate) fn sorted_entry(&self, name: &str) -> Option<BaseEntry> {
        self.dir
            .get(name)
            .filter(|entry| matches!(entry.variant, VARIANT_INT64 | VARIANT_STRING))
            .copied()
    }

    /// Whether `name` is searched in the mapping and edited through a delta.
    pub(crate) fn is_sorted(&self, name: &str) -> bool {
        self.sorted_entry(name).is_some()
    }

    /// `(keys, nodes)` of a string entry, in the layout of [`string_keys`].
    fn string_parts_of(&self, entry: &BaseEntry) -> Option<(&[u8], &[u8])> {
        if entry.variant != VARIANT_STRING {
            return None;
        }
        let IdSource::Sections(sections) = &self.source else {
            return None;
        };
        let (keys, nodes) = sections.get(entry.section as usize)?;
        Some((keys.as_bytes(), nodes.as_bytes()))
    }

    /// Binary search of a string entry, in the mapping. Only a string id
    /// matches: a string has no other spelling.
    #[inline(never)]
    pub(crate) fn lookup_string_at(&self, entry: &BaseEntry, id: &Value) -> Option<NodeIndex> {
        let Value::String(wanted) = id else {
            return None;
        };
        let (keys, nodes) = self.string_parts_of(entry)?;
        let position = string_keys::search(keys, entry.num_entries as usize, wanted.as_bytes())?;
        Some(NodeIndex::new(read_le_u32(nodes, position)? as usize))
    }

    /// [`Self::lookup`] on a mapped-search `entry` already in hand.
    pub(crate) fn lookup_sorted_at(&self, entry: &BaseEntry, id: &Value) -> Option<NodeIndex> {
        match entry.variant {
            VARIANT_STRING => self.lookup_string_at(entry, id),
            _ => self.lookup_int64_at(entry, id),
        }
    }

    /// [`Self::lookup_exact`] on a mapped-search `entry` already in hand.
    pub(crate) fn lookup_sorted_exact_at(
        &self,
        entry: &BaseEntry,
        id: &Value,
    ) -> Option<NodeIndex> {
        match entry.variant {
            VARIANT_STRING => self.lookup_string_at(entry, id),
            _ => self.lookup_int64_exact_at(entry, id),
        }
    }

    /// `(keys, node indices)` of an Int64Sorted entry — little-endian `i64`s and
    /// `u32`s, both sorted by key — or `None` for any other variant.
    pub(crate) fn int64_parts(&self, name: &str) -> Option<(&[u8], &[u8])> {
        self.int64_parts_of(&self.int64_entry(name)?)
    }

    fn int64_parts_of(&self, entry: &BaseEntry) -> Option<(&[u8], &[u8])> {
        if let IdSource::Sections(sections) = &self.source {
            let (keys, nodes) = sections.get(entry.section as usize)?;
            return Some((keys.as_bytes(), nodes.as_bytes()));
        }
        let keys_len = (entry.num_entries as usize).checked_mul(8)?;
        let start = usize::try_from(entry.payload_off).ok()?;
        let end = start.checked_add(usize::try_from(entry.payload_len).ok()?)?;
        self.file_bytes()?
            .get(start..end)?
            .split_at_checked(keys_len)
    }

    /// The whole mapped file, for a base that is one; a section base holds only
    /// Integer and Int64Sorted entries, which read through their own parts.
    fn file_bytes(&self) -> Option<&[u8]> {
        match &self.source {
            IdSource::File { mmap, .. } => Some(mmap),
            IdSource::Sections(_) => None,
        }
    }

    /// The file this base maps, when it is a disk graph's in the current
    /// layout: one a save may link instead of rewriting.
    fn current_origin(&self) -> Option<&Path> {
        match &self.source {
            IdSource::File {
                origin, version, ..
            } if *version == VERSION => Some(origin),
            _ => None,
        }
    }

    /// A base over the id sections of a v9 `.kgl`: `(type name, keys, nodes)`
    /// triples where `keys` holds ascending, unique little-endian ids (`u32`
    /// for an Integer entry, `i64` for an Int64Sorted one, by the section's
    /// element type) and `nodes` the little-endian `u32` node index of each,
    /// every node below `node_bound`. Each section's CRC32 is checked here, so
    /// a damaged index is refused at load instead of answering a lookup
    /// wrongly.
    pub(crate) fn from_sections(
        triples: Vec<(String, MappedSection, MappedSection)>,
        node_bound: usize,
    ) -> std::io::Result<Self> {
        use crate::graph::io::file::mappable::SectionDtype;
        let mut dir = HashMap::with_capacity(triples.len());
        let mut sections = Vec::with_capacity(triples.len());
        for (name, keys, nodes) in triples {
            keys.verify()?;
            nodes.verify()?;
            if keys.dtype == SectionDtype::U8 {
                let count = string_keys::validate(keys.as_bytes(), nodes.as_bytes(), node_bound)?;
                let num_entries =
                    u32::try_from(count).map_err(|_| invalid_index("entry count exceeds u32"))?;
                let payload_len = (keys.as_bytes().len() + nodes.as_bytes().len()) as u64;
                keys.release_resident();
                nodes.release_resident();
                let entry = BaseEntry {
                    variant: VARIANT_STRING,
                    num_entries,
                    payload_off: 0,
                    payload_len,
                    section: sections.len() as u32,
                };
                if dir.insert(name, entry).is_some() {
                    return Err(invalid_index("duplicate type name"));
                }
                sections.push((keys, nodes));
                continue;
            }
            let (variant, width) = match keys.dtype {
                SectionDtype::U32 => (VARIANT_INTEGER, 4),
                SectionDtype::I64 => (VARIANT_INT64, 8),
                _ => return Err(invalid_index("an id section has an unsupported key type")),
            };
            let (key_bytes, node_bytes) = (keys.as_bytes(), nodes.as_bytes());
            let count = key_bytes.len() / width;
            if !key_bytes.len().is_multiple_of(width) || node_bytes.len() != count * 4 {
                return Err(invalid_index(
                    "an id section pair is not parallel runs of keys and u32 nodes",
                ));
            }
            let mut previous = None;
            for index in 0..count {
                let key = if variant == VARIANT_INTEGER {
                    i64::from(read_le_u32(key_bytes, index).unwrap())
                } else {
                    read_le_i64(key_bytes, index).unwrap()
                };
                if previous.is_some_and(|prior| key <= prior) {
                    return Err(invalid_index("keys are not strictly increasing"));
                }
                previous = Some(key);
                if read_le_u32(node_bytes, index).unwrap() as usize >= node_bound {
                    return Err(invalid_index("an id names a node outside the graph"));
                }
            }
            let num_entries =
                u32::try_from(count).map_err(|_| invalid_index("entry count exceeds u32"))?;
            keys.release_resident();
            nodes.release_resident();
            let entry = BaseEntry {
                variant,
                num_entries,
                payload_off: 0,
                payload_len: (count * (width + 4)) as u64,
                section: sections.len() as u32,
            };
            if dir.insert(name, entry).is_some() {
                return Err(invalid_index("duplicate type name"));
            }
            sections.push((keys, nodes));
        }
        Ok(Self {
            source: IdSource::Sections(sections),
            dir,
            general_cache: RwLock::new(HashMap::new()),
        })
    }

    /// Binary search of an Int64Sorted entry, in the mapping.
    fn search_int64_at(&self, entry: &BaseEntry, wanted: i64) -> Option<NodeIndex> {
        let (keys, nodes) = self.int64_parts_of(entry)?;
        let position = le_i64_binary_search(keys, wanted)?;
        Some(NodeIndex::new(read_le_u32(nodes, position)? as usize))
    }

    /// [`Self::lookup`] on an Int64Sorted `entry` already in hand: the integer
    /// the id denotes (`UniqueId`, `Int64` and a whole `Float64` alike).
    pub(crate) fn lookup_int64_at(&self, entry: &BaseEntry, id: &Value) -> Option<NodeIndex> {
        self.search_int64_at(entry, crate::graph::schema::id_integer(id)?)
    }

    /// [`Self::lookup_exact`] on an Int64Sorted `entry` already in hand.
    pub(crate) fn lookup_int64_exact_at(&self, entry: &BaseEntry, id: &Value) -> Option<NodeIndex> {
        match id {
            Value::Int64(wanted) => self.search_int64_at(entry, *wanted),
            _ => None,
        }
    }

    /// Load `id_indices.bin` from `dir`. Returns `Ok(None)` if absent, shorter
    /// than the header, or magic mismatch.
    pub fn load_from(dir: &Path, interner: &StringInterner) -> std::io::Result<Option<Self>> {
        let path = dir.join("id_indices.bin");
        if !path.exists() {
            return Ok(None);
        }
        let file = std::fs::File::open(&path)?;
        let len = file.metadata()?.len() as usize;
        if len < HEADER_BYTES {
            return Ok(None);
        }
        // SAFETY: GraphDirectoryLock serializes disk-graph writers, which
        // publish a new immutable generation instead of truncating the
        // generation selected by this reader. This inode therefore remains
        // stable for the mapping's lifetime.
        let mmap = unsafe { Mmap::map(&file)? };
        if &mmap[..8] != MAGIC {
            return Ok(None);
        }
        let version = u32::from_le_bytes(mmap[8..12].try_into().unwrap());
        match version {
            1 => {
                return Err(crate::graph::io::file::pre_014_bincode_error(
                    "id_indices.bin v1",
                ));
            }
            VERSION | VERSION_2 => {}
            _ => return Err(invalid_index("unsupported raw index version")),
        }
        let num_types = u32::from_le_bytes(mmap[12..16].try_into().unwrap()) as usize;
        let dir_offset = usize::try_from(u64::from_le_bytes(mmap[16..24].try_into().unwrap()))
            .map_err(|_| invalid_index("directory offset exceeds usize"))?;
        let data_offset = usize::try_from(u64::from_le_bytes(mmap[24..32].try_into().unwrap()))
            .map_err(|_| invalid_index("data offset exceeds usize"))?;
        let dir_bytes = DIR_ENTRY_BYTES
            .checked_mul(num_types)
            .ok_or_else(|| invalid_index("directory size overflow"))?;
        let need = dir_offset
            .checked_add(dir_bytes)
            .ok_or_else(|| invalid_index("directory range overflow"))?;
        if dir_offset != HEADER_BYTES || data_offset != need || need > len {
            return Err(invalid_index("invalid directory/data boundary"));
        }

        let mut dir_map: HashMap<String, BaseEntry> = HashMap::with_capacity(num_types);
        let mut general_cache_map = HashMap::new();
        let mut previous_key = None;
        let mut expected_payload = data_offset;
        for i in 0..num_types {
            let off = dir_offset + i * DIR_ENTRY_BYTES;
            let type_key = u64::from_le_bytes(mmap[off..off + 8].try_into().unwrap());
            let variant = mmap[off + 8];
            let num_entries_u64 = u64::from_le_bytes(mmap[off + 16..off + 24].try_into().unwrap());
            let payload_off = u64::from_le_bytes(mmap[off + 24..off + 32].try_into().unwrap());
            let payload_len = u64::from_le_bytes(mmap[off + 32..off + 40].try_into().unwrap());
            if previous_key.is_some_and(|previous| type_key <= previous) {
                return Err(invalid_index("directory keys are not strictly increasing"));
            }
            previous_key = Some(type_key);
            if !matches!(variant, VARIANT_INTEGER | VARIANT_GENERAL | VARIANT_INT64) {
                return Err(invalid_index("directory contains an unknown variant"));
            }
            let num_entries = u32::try_from(num_entries_u64)
                .map_err(|_| invalid_index("entry count exceeds u32"))?;
            let payload_off_usize = usize::try_from(payload_off)
                .map_err(|_| invalid_index("payload offset exceeds usize"))?;
            let payload_len_usize = usize::try_from(payload_len)
                .map_err(|_| invalid_index("payload length exceeds usize"))?;
            let payload_end = payload_off_usize
                .checked_add(payload_len_usize)
                .ok_or_else(|| invalid_index("payload range overflow"))?;
            if payload_off_usize != expected_payload || payload_end > len {
                return Err(invalid_index(
                    "payloads overlap, contain gaps, or exceed the file",
                ));
            }
            if variant == VARIANT_INTEGER {
                let expected_len = num_entries_u64
                    .checked_mul(8)
                    .ok_or_else(|| invalid_index("integer payload size overflow"))?;
                if payload_len != expected_len {
                    return Err(invalid_index("integer payload has invalid size"));
                }
                let keys_end = payload_off_usize + num_entries as usize * 4;
                let mut previous = None;
                for index in 0..num_entries as usize {
                    let key = read_le_u32(&mmap[payload_off_usize..keys_end], index).unwrap();
                    if previous.is_some_and(|prior| key <= prior) {
                        return Err(invalid_index("integer keys are not strictly increasing"));
                    }
                    previous = Some(key);
                }
            } else if variant == VARIANT_INT64 {
                let expected_len = num_entries_u64
                    .checked_mul(INT64_ENTRY_BYTES as u64)
                    .ok_or_else(|| invalid_index("int64 payload size overflow"))?;
                if payload_len != expected_len {
                    return Err(invalid_index("int64 payload has invalid size"));
                }
                let keys_end = payload_off_usize + num_entries as usize * 8;
                let mut previous = None;
                for index in 0..num_entries as usize {
                    let key = read_le_i64(&mmap[payload_off_usize..keys_end], index).unwrap();
                    if previous.is_some_and(|prior| key <= prior) {
                        return Err(invalid_index("int64 keys are not strictly increasing"));
                    }
                    previous = Some(key);
                }
            } else if payload_len > MAX_GENERAL_INDEX_DECODE_BYTES {
                return Err(invalid_index("general payload exceeds decode limit"));
            }
            expected_payload = payload_end;
            let Some(name) = interner.try_resolve(InternedKey::from_u64(type_key)) else {
                // Directories written before the writer resolved its keys
                // (through 0.15.0) carry entries for type names the interner
                // sidecar never received — a type declared with no rows, or a
                // label a query merely mentioned. Those entries are always
                // empty, and an id index is a cache the read path rebuilds on
                // demand, so dropping one recovers the graph at no cost rather
                // than making the whole directory unreadable. A *populated*
                // entry under an unresolvable key is not that: it is a
                // mismatched or damaged sidecar, and still fails the load.
                if num_entries == 0 {
                    continue;
                }
                return Err(invalid_index("directory contains an unresolved type key"));
            };
            if variant == VARIANT_GENERAL {
                let blob = &mmap[payload_off_usize..payload_end];
                let map: FxHashMap<Value, NodeIndex> = serde_codec::decode_exact_with(
                    serde_codec::CURRENT_CODEC,
                    blob,
                    blob.len() as u64,
                    serde_codec::DecodeLimits::new(
                        MAX_GENERAL_INDEX_DECODE_BYTES,
                        MAX_GENERAL_INDEX_DECODE_BYTES,
                    ),
                )
                .map_err(|_| invalid_index("general payload Postcard is malformed"))?;
                if map.len() != num_entries as usize {
                    return Err(invalid_index(
                        "general payload has duplicate or missing keys",
                    ));
                }
                general_cache_map.insert(name.to_string(), Arc::new(heal_general_spellings(map)));
            }
            if dir_map
                .insert(
                    name.to_string(),
                    BaseEntry {
                        variant,
                        num_entries,
                        payload_off,
                        payload_len,
                        section: 0,
                    },
                )
                .is_some()
            {
                return Err(invalid_index("duplicate resolved type name"));
            }
        }
        if expected_payload != len {
            return Err(invalid_index(
                "payload directory does not cover the file exactly",
            ));
        }

        Ok(Some(Self {
            source: IdSource::File {
                mmap: Arc::new(mmap),
                origin: path,
                version,
                _pin: super::generation::GenerationPin::containing(dir),
            },
            dir: dir_map,
            general_cache: RwLock::new(general_cache_map),
        }))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.dir.contains_key(name)
    }

    pub fn lookup(&self, name: &str, id: &Value) -> Option<NodeIndex> {
        let entry = self.dir.get(name)?;
        match entry.variant {
            VARIANT_INTEGER => self.lookup_integer(entry, id),
            VARIANT_GENERAL => self.lookup_general(name, entry, id),
            // An Int64-only General index answers a query through the integer
            // it denotes (`UniqueId`, `Int64` and a whole `Float64` alike) and
            // matches nothing else, so the same rule is one search here.
            VARIANT_INT64 => self.lookup_int64_at(entry, id),
            VARIANT_STRING => self.lookup_string_at(entry, id),
            _ => None,
        }
    }

    /// Materialize a base entry into an owned `TypeIdIndex`: an entry heap map.
    ///
    /// **A full map, one heap entry per id.** It is for the callers that need
    /// every id at once — the N-Triples export walking the whole index, and the
    /// fallbacks that must hand a heap index to code written against one. A
    /// caller that resolves ids one at a time uses [`Self::lookup`] and never
    /// pays this; [`IdIndexStore::entry_or_default`] layers a delta over an
    /// Int64Sorted entry instead of calling it.
    pub fn materialize(&self, name: &str) -> Option<TypeIdIndex> {
        let entry = self.dir.get(name)?;
        #[cfg(test)]
        FULL_MAPS_BUILT.with(|count| count.set(count.get() + 1));
        match entry.variant {
            VARIANT_INTEGER => {
                let (keys, idxs) = self.integer_bytes(entry)?;
                let mut map: FxHashMap<u32, NodeIndex> = FxHashMap::with_capacity_and_hasher(
                    entry.num_entries as usize,
                    Default::default(),
                );
                for index in 0..entry.num_entries as usize {
                    map.insert(
                        read_le_u32(keys, index)?,
                        NodeIndex::new(read_le_u32(idxs, index)? as usize),
                    );
                }
                Some(TypeIdIndex::Integer(map))
            }
            VARIANT_GENERAL => {
                let map = self.general_map(name, entry)?;
                Some(TypeIdIndex::General((*map).clone()))
            }
            VARIANT_INT64 => {
                let (keys, nodes) = self.int64_parts(name)?;
                let mut map: FxHashMap<Value, NodeIndex> = FxHashMap::with_capacity_and_hasher(
                    entry.num_entries as usize,
                    Default::default(),
                );
                for index in 0..entry.num_entries as usize {
                    map.insert(
                        Value::Int64(read_le_i64(keys, index)?),
                        NodeIndex::new(read_le_u32(nodes, index)? as usize),
                    );
                }
                Some(TypeIdIndex::General(map))
            }
            VARIANT_STRING => {
                let (keys, nodes) = self.string_parts_of(entry)?;
                let count = entry.num_entries as usize;
                let mut map: FxHashMap<Value, NodeIndex> =
                    FxHashMap::with_capacity_and_hasher(count, Default::default());
                for index in 0..count {
                    let key = string_keys::key_at(keys, count, index)?;
                    map.insert(
                        Value::String(String::from_utf8(key.to_vec()).ok()?),
                        NodeIndex::new(read_le_u32(nodes, index)? as usize),
                    );
                }
                Some(TypeIdIndex::General(map))
            }
            _ => None,
        }
    }

    fn integer_bytes(&self, entry: &BaseEntry) -> Option<(&[u8], &[u8])> {
        if let IdSource::Sections(sections) = &self.source {
            let (keys, nodes) = sections.get(entry.section as usize)?;
            return Some((keys.as_bytes(), nodes.as_bytes()));
        }
        let n = entry.num_entries as usize;
        let off = entry.payload_off as usize;
        let half = n * 4;
        if entry.payload_len != (half * 2) as u64 {
            return None;
        }
        let bytes = self.file_bytes()?.get(off..off + half * 2)?;
        Some(bytes.split_at(half))
    }

    fn lookup_integer(&self, entry: &BaseEntry, id: &Value) -> Option<NodeIndex> {
        let key_u32 = id_u32(id)?;
        let (keys, idxs) = self.integer_bytes(entry)?;
        let index = le_u32_binary_search(keys, key_u32)?;
        Some(NodeIndex::new(read_le_u32(idxs, index)? as usize))
    }

    fn lookup_general(&self, name: &str, entry: &BaseEntry, id: &Value) -> Option<NodeIndex> {
        let map = self.general_map(name, entry)?;
        map.get(id)
            .or_else(|| id_spellings(id).find_map(|key| map.get(&key)))
            .copied()
    }

    fn general_map(
        &self,
        name: &str,
        entry: &BaseEntry,
    ) -> Option<Arc<FxHashMap<Value, NodeIndex>>> {
        if let Some(arc) = self.general_cache.read().unwrap().get(name).cloned() {
            return Some(arc);
        }
        let off = entry.payload_off as usize;
        let len = entry.payload_len as usize;
        let blob = self.file_bytes()?.get(off..off + len)?;
        let map: FxHashMap<Value, NodeIndex> = serde_codec::decode_exact_with(
            serde_codec::CURRENT_CODEC,
            blob,
            blob.len() as u64,
            serde_codec::DecodeLimits::new(
                MAX_GENERAL_INDEX_DECODE_BYTES,
                MAX_GENERAL_INDEX_DECODE_BYTES,
            ),
        )
        .ok()?;
        if map.len() != entry.num_entries as usize {
            return None;
        }
        let arc = Arc::new(heal_general_spellings(map));
        self.general_cache
            .write()
            .unwrap()
            .insert(name.to_string(), Arc::clone(&arc));
        Some(arc)
    }
}

/// HashMap-shaped wrapper around an optional mmap base + in-memory overlay.
///
/// Reads consult overlay first (covers post-load mutations), then base.
/// Mutations only ever land in overlay; `removed` tracks types that the
/// caller explicitly cleared so that base entries are masked.
#[derive(Default)]
pub struct IdIndexStore {
    /// In-memory layer: indices built/mutated post-load, plus lazily-cached
    /// indices the read path builds on a miss. Behind a `RwLock` so the
    /// read path can build + cache through `&self` — `DirGraph` is shared
    /// as `Arc<DirGraph>` and reads run on multiple threads (GIL-release),
    /// so this must be thread-safe.
    overlay: RwLock<HashMap<String, TypeEntry>>,
    /// Types that exist in `base` but were removed/invalidated post-load.
    removed: std::collections::HashSet<String>,
    base: Option<Arc<IdIndexBase>>,
    /// Lazily built indexes shared by every clone of this store — see
    /// `id_index_shared`.
    shared: Arc<SharedIdIndexes>,
}

impl IdIndexStore {
    /// Whether the store is exactly its file-served base: nothing built or
    /// written over it.
    #[cfg(test)]
    pub(crate) fn is_served_unmodified(&self) -> bool {
        self.base.is_some() && self.removed.is_empty() && self.overlay.read().unwrap().is_empty()
    }
}

impl Clone for IdIndexStore {
    /// **The fork seam for `id_indices`.**
    ///
    /// Instead of deep-copying a map with one entry per node of every
    /// materialised type — 3.7 ms at 1M, and 90% of what a plain graph's fork
    /// still cost at that point — this converts each of *our own* entries into
    /// a shared base in place and hands the child an empty delta over the same
    /// allocation. Both graphs then read identical content; only the
    /// representation changed.
    ///
    /// It has to happen here, taking the write lock through `&self`, because
    /// every fork reaches this field as a `&self` clone: by the time write
    /// entry holds a `&mut DirGraph` the copy has already been made. That is
    /// why `overlay`'s `RwLock` is load-bearing beyond thread safety.
    fn clone(&self) -> Self {
        let mut overlay = self.overlay.write().unwrap();
        let shared: HashMap<String, TypeEntry> = overlay
            .iter_mut()
            .map(|(name, entry)| (name.clone(), TypeEntry::layered_over(entry.share())))
            .collect();
        Self {
            overlay: RwLock::new(shared),
            removed: self.removed.clone(),
            base: self.base.clone(),
            shared: Arc::clone(&self.shared),
        }
    }
}

impl IdIndexStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_base(base: IdIndexBase) -> Self {
        Self {
            overlay: RwLock::new(HashMap::new()),
            removed: std::collections::HashSet::new(),
            base: Some(Arc::new(base)),
            shared: Arc::default(),
        }
    }

    /// After a save publishes `id_indices.bin`, serve every type it covers from
    /// that mapping and drop the heap entries it made redundant, so a saved
    /// graph holds no heap copy of an index it just wrote. A type the writer
    /// skipped (its ids repeat, or it was an empty cache entry) keeps its
    /// overlay entry.
    ///
    /// Installs nothing unless every overlay entry the file covers has the same
    /// id count as the file's entry. That catches a file that lost or gained
    /// ids; it does not compare the ids themselves.
    pub fn rebase_onto(&mut self, base: IdIndexBase) -> Result<(), String> {
        for (name, entry) in self.overlay.get_mut().unwrap().iter() {
            if base.contains(name) && base.entry_len(name) != Some(entry.len()) {
                return Err(format!(
                    "the published id index for type '{name}' differs from the live one"
                ));
            }
        }
        self.overlay
            .get_mut()
            .unwrap()
            .retain(|name, _| !base.contains(name));
        self.removed.clear();
        self.base = Some(Arc::new(base));
        Ok(())
    }

    /// The file this store serves, when nothing has been written over it: no
    /// heap entry (a mutated type, or a cache a read built for a type the file
    /// lacks) and no removed type. Such a store would write a file with the
    /// same content, so a save may link the file instead. A file in an older
    /// layout is not offered: the next save is what rewrites it in the current
    /// one.
    pub(crate) fn unchanged_origin(&self) -> Option<&Path> {
        let untouched = self.removed.is_empty() && self.overlay.read().unwrap().is_empty();
        let origin = self.base.as_deref()?.current_origin()?;
        untouched.then_some(origin)
    }

    pub fn contains_key(&self, name: &str) -> bool {
        if self.overlay.read().unwrap().contains_key(name) {
            return true;
        }
        if self.removed.contains(name) {
            return false;
        }
        self.base.as_ref().is_some_and(|b| b.contains(name))
    }

    /// Look up `id` for `name`. If the type isn't indexed anywhere (neither
    /// overlay nor base, or it was invalidated), build the index via `build`
    /// — which scans the graph — and cache it in the overlay, so the read
    /// path is O(1) on every subsequent lookup. The build runs at most once
    /// per type until the next invalidation. Returns None when the id simply
    /// isn't present (no scan).
    ///
    /// `generation` is the type's member-bucket generation
    /// (`TypeIndexStore::generation`), or `None` to build without sharing. With
    /// it the build is published to every clone of this store and an index a
    /// clone already built at the same generation is used instead of scanning —
    /// see `id_index_shared`.
    ///
    /// Without this, the read path (`MATCH (n {id:X})`, `MERGE` match) falls
    /// back to a full scan whenever the index is absent — after `add_nodes` /
    /// `CREATE` / `DELETE`. (issue #20)
    pub fn lookup_or_build(
        &self,
        name: &str,
        id: &Value,
        generation: Option<u64>,
        build: impl FnOnce() -> TypeIdIndex,
    ) -> Option<NodeIndex> {
        {
            let ov = self.overlay.read().unwrap();
            if let Some(idx) = ov.get(name) {
                return idx.get(id);
            }
        }
        if !self.removed.contains(name) {
            if let Some(base) = self.base.as_deref() {
                if base.contains(name) {
                    return base.lookup(name, id);
                }
            }
        }
        // Not indexed anywhere — build once and cache (idempotent under a
        // concurrent race: the first writer wins, both indices are equal).
        let built = self.lazy_entry(name, generation, build);
        let mut ov = self.overlay.write().unwrap();
        ov.entry(name.to_string()).or_insert(built).get(id)
    }

    /// The entry a lazy build installs: layered over the shared index when the
    /// type has a generation, owned otherwise.
    fn lazy_entry(
        &self,
        name: &str,
        generation: Option<u64>,
        build: impl FnOnce() -> TypeIdIndex,
    ) -> TypeEntry {
        match generation {
            Some(generation) => {
                TypeEntry::layered_over(self.shared.get_or_publish(name, generation, build))
            }
            None => TypeEntry::from(build()),
        }
    }

    /// Ensure `name` is indexed (overlay or base) — the `&self` pre-warm
    /// counterpart of the self-healing read path, so callers can pre-build an
    /// id index without `&mut` and its `Arc::make_mut` deep copy. Racing
    /// builders are harmless: the first writer wins and both indices are equal.
    pub fn ensure(&self, name: &str, generation: Option<u64>, build: impl FnOnce() -> TypeIdIndex) {
        if self.contains_key(name) {
            return;
        }
        let built = self.lazy_entry(name, generation, build);
        let mut ov = self.overlay.write().unwrap();
        ov.entry(name.to_string()).or_insert(built);
    }

    /// Look up without building — None when the type isn't indexed.
    pub fn lookup(&self, name: &str, id: &Value) -> Option<NodeIndex> {
        {
            let ov = self.overlay.read().unwrap();
            if let Some(idx) = ov.get(name) {
                return idx.get(id);
            }
        }
        if self.removed.contains(name) {
            return None;
        }
        self.base.as_deref().and_then(|b| {
            if b.contains(name) {
                b.lookup(name, id)
            } else {
                None
            }
        })
    }

    /// Exact stored-value lookup plus the live entry count, after the caller
    /// has ensured this type's index exists. Kept internal for outline root
    /// disambiguation; ordinary id lookup retains its numeric normalization.
    pub(crate) fn lookup_exact_with_len(
        &self,
        name: &str,
        id: &Value,
    ) -> Option<(Option<NodeIndex>, usize)> {
        {
            let overlay = self.overlay.read().unwrap();
            if let Some(index) = overlay.get(name) {
                return Some((index.get_exact(id), index.len()));
            }
        }
        if self.removed.contains(name) {
            return None;
        }
        let base = self.base.as_deref()?;
        Some((base.lookup_exact(name, id), base.entry_len(name)?))
    }

    /// Borrow `source`'s and `target`'s **overlay-resident** id indices in
    /// place for the length of one bulk pass, and run `f` against them.
    ///
    /// This is the bulk-pass counterpart to materializing a type's whole map:
    /// one lock acquisition and two borrowed entries, where materializing pays a
    /// map insert per node *of the whole type* before the first row is looked
    /// at. On a property-free `add_connections` at 100k nodes / 24k edges,
    /// materializing was 53% of the call (samply, 2026-08-15) and grew with
    /// the graph while the row count stayed fixed.
    ///
    /// Returns `None` — without calling `f` — unless **both** types are in the
    /// overlay, which is every heap-resident graph and every type a loaded
    /// graph has since mutated. A base (mmap) entry is deliberately excluded:
    /// a caller resolves such a type row by row through [`Self::lookup`], a
    /// binary search over the mapped file.
    ///
    /// `f` must not re-enter the store — the read lock is held for its whole
    /// execution.
    pub fn with_overlay_type_pair<R>(
        &self,
        source: &str,
        target: &str,
        f: impl FnOnce(&TypeEntry, &TypeEntry) -> R,
    ) -> Option<R> {
        let overlay = self.overlay.read().unwrap();
        let source_entry = overlay.get(source)?;
        let target_entry = if source == target {
            source_entry
        } else {
            overlay.get(target)?
        };
        Some(f(source_entry, target_entry))
    }

    pub fn insert(&mut self, name: String, idx: TypeIdIndex) {
        self.shared.forget(&name);
        self.removed.remove(&name);
        self.overlay
            .get_mut()
            .unwrap()
            .insert(name, TypeEntry::from(idx));
    }

    /// Number of ids indexed for `name` in the mutable overlay, or `None` when
    /// the type is not overlay-resident: what a test reads to see whether a save
    /// or a load left a heap entry behind.
    #[cfg(test)]
    pub fn overlay_len(&self, name: &str) -> Option<usize> {
        self.overlay
            .read()
            .unwrap()
            .get(name)
            .map(|entry| entry.len())
    }

    /// Number of ids indexed for `name` where [`Self::evict_entries`] can edit
    /// the index in place, or `None` where it cannot: the type's overlay entry,
    /// or an Int64Sorted or string entry of the mapped indexes, which an edit layers a delta
    /// over ([`Self::entry_or_default`]) and leaves in the mapping. Any other
    /// base variant would be copied onto the heap to edit, which is no cheaper
    /// than the rebuild an invalidation causes.
    pub fn editable_len(&self, name: &str) -> Option<usize> {
        if let Some(entry) = self.overlay.read().unwrap().get(name) {
            return Some(entry.len());
        }
        if self.removed.contains(name) {
            return None;
        }
        let base = self.base.as_deref()?;
        base.is_sorted(name).then(|| base.entry_len(name))?
    }

    /// Drop `entries` (`id -> node`) from `name`'s index in place, instead of
    /// invalidating the whole type.
    ///
    /// Deleting one node used to `remove()` the entire type index, so the next
    /// id lookup rebuilt it by scanning every node of the type — an O(N_type)
    /// cost charged to a single-node delete. The create path maintains the
    /// index incrementally the same way (the `pk_id` match in the create
    /// executor).
    ///
    /// Falls back to whole-type invalidation, and returns `false`, when
    /// [`Self::editable_len`] has nothing to edit: an unbuilt type, or a type
    /// served from the file in a variant that would have to be copied onto the
    /// heap. An Int64Sorted entry served from the mapping is edited through a
    /// delta over it, so the deleted ids become tombstones and nothing is
    /// rebuilt. Each entry is removed only if it still resolves to the given
    /// node, so a re-pointed id is left intact.
    ///
    /// The caller is responsible for the duplicate-id precondition: this edits
    /// exactly the ids it is given, whereas a rebuild re-derives the whole map
    /// and would surface a shadowed duplicate. See `detach_delete_nodes`.
    pub fn evict_entries(&mut self, name: &str, entries: &[(Value, NodeIndex)]) -> bool {
        if self.editable_len(name).is_none() {
            self.remove(name);
            return false;
        }
        let entry = self.entry_or_default(name.to_string());
        for (id, idx) in entries {
            entry.remove_matching(id, *idx);
        }
        true
    }

    /// Drop `name`'s index; the next read rebuilds it from the graph.
    pub fn remove(&mut self, name: &str) {
        self.shared.forget(name);
        self.overlay.get_mut().unwrap().remove(name);
        if self.base.as_ref().is_some_and(|b| b.contains(name)) {
            self.removed.insert(name.to_string());
        }
    }

    pub fn clear(&mut self) {
        self.shared.forget_all();
        self.overlay.get_mut().unwrap().clear();
        if let Some(base) = &self.base {
            self.removed.extend(base.dir.keys().cloned());
        }
    }

    pub fn len(&self) -> usize {
        let overlay = self.overlay.read().unwrap();
        let base_count = self
            .base
            .as_ref()
            .map(|b| b.dir.keys().filter(|k| !self.removed.contains(*k)).count())
            .unwrap_or(0);
        let overlay_only = overlay
            .keys()
            .filter(|k| self.base.as_ref().map(|b| !b.contains(k)).unwrap_or(true))
            .count();
        base_count + overlay_only
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Owned snapshot of every live `TypeIdIndex` (overlay first, then base
    /// entries that aren't shadowed/removed): **a full heap map per type**, for
    /// the N-Triples build, which walks every id of every type. Returns owned
    /// indices because the read lock can't be held across the caller's
    /// iteration. A save does not use it; see [`write_id_indices_bin`].
    pub fn values(&self) -> Vec<TypeIdIndex> {
        self.snapshot().into_iter().map(|(_, v)| v).collect()
    }

    fn snapshot(&self) -> Vec<(String, TypeIdIndex)> {
        let overlay = self.overlay.read().unwrap();
        let mut out: Vec<(String, TypeIdIndex)> = overlay
            .iter()
            .map(|(k, v)| (k.clone(), v.materialize()))
            .collect();
        if let Some(base) = self.base.as_deref() {
            for k in base.dir.keys() {
                if !overlay.contains_key(k.as_str()) && !self.removed.contains(k.as_str()) {
                    if let Some(materialized) = base.materialize(k) {
                        out.push((k.clone(), materialized));
                    }
                }
            }
        }
        out
    }

    /// HashMap-`entry`-shaped accessor: bring a base entry into the overlay (or
    /// default-construct one), then hand back a `&mut` to it. Used wherever the
    /// index is maintained incrementally: the N-Triples and RDF loaders'
    /// per-entity build, and the create paths. `&mut self` gives exclusive
    /// access, so `get_mut()` is uncontended (no lock cost).
    ///
    /// An Int64Sorted base entry comes in as a delta over the mapping
    /// ([`TypeEntry::OverBase`]) — the first write costs nothing in proportion
    /// to the type's size. Any other base entry is copied onto the heap first.
    pub fn entry_or_default(&mut self, name: String) -> &mut TypeEntry {
        self.shared.forget(&name);
        let needs_promotion = {
            let overlay = self.overlay.get_mut().unwrap();
            !overlay.contains_key(&name) && !self.removed.contains(&name)
        };
        if needs_promotion {
            if let Some(base) = self.base.as_ref() {
                let promoted = TypeEntry::over_base(Arc::clone(base), &name)
                    .or_else(|| base.materialize(&name).map(TypeEntry::from));
                if let Some(entry) = promoted {
                    self.overlay.get_mut().unwrap().insert(name.clone(), entry);
                }
            }
        }
        self.removed.remove(&name);
        self.overlay.get_mut().unwrap().entry(name).or_default()
    }

    /// Fold every shared base back in where this graph is its last holder.
    ///
    /// Called at write entry or successful Session publication beside
    /// `GraphBackend::try_compact`, so the "hold a view, write, drop the view,
    /// write again" sequence returns to the flat representation on the very next
    /// write. Per entry the fold is a plain map overwrite: unlike the topology
    /// overlay there is no slot to predict, because the delta already recorded the
    /// real `NodeIndex` values the graph handed out. What it must not do is edit a
    /// base another graph is reading, which is what `Arc::get_mut` inside
    /// `TypeEntry::try_compact` gates.
    pub fn try_compact(&mut self) {
        for entry in self.overlay.get_mut().unwrap().values_mut() {
            entry.try_compact();
        }
    }

    /// Replace the entire store with a fresh HashMap (used by load fallback
    /// for legacy `.bin.zst`-only graphs and by `reindex()`).
    pub fn replace_with(&mut self, map: HashMap<String, TypeIdIndex>) {
        self.shared.forget_all();
        *self.overlay.get_mut().unwrap() = map
            .into_iter()
            .map(|(name, index)| (name, TypeEntry::from(index)))
            .collect();
        self.removed.clear();
        self.base = None;
    }
}

pub(crate) mod string_keys;
mod write;
pub use write::write_id_indices_bin;

#[cfg(test)]
mod int64_tests;
#[cfg(test)]
mod string_tests;
#[cfg(test)]
mod validation_tests;
