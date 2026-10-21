//! Derived indexes in a v9 container's data region: node type membership
//! and the id index.
//!
//! These are caches a load would otherwise rebuild from the topology (an
//! O(nodes) grouping pass, a heap hash map per id on first lookup). A save of
//! a large graph
//! records them; a load into mapped storage serves them from the file instead
//! of the heap. A load into memory storage ignores them and builds exactly
//! what it always built.
//!
//! Sections (owner `indexes`, little-endian):
//! - `idx:type:<T>`: `u32` node indices of type `T`, ascending (kind 6).
//! - `idx:id:<T>:keys` / `idx:id:<T>:nodes`: the sorted unique ids of a type
//!   whose ids are all `UniqueId` (`u32` keys) or all `Int64` (`i64` keys), and
//!   the `u32` node of each (kinds 7 and 8). Any other id leaves the type to
//!   the lazy build.
//!
//! A section that fails its CRC32 or its structure checks is a damaged file
//! and refuses the load; a type section set whose member count disagrees with
//! the decoded topology is dropped and the indexes are rebuilt. Node writes
//! layer over the id and type indexes, as they do for a disk graph.

use rustc_hash::FxHashMap;

use super::embedding_sections::{escape, unescape};
use super::load_options::MapPolicy;
use super::mappable::{
    resolve_mapping, MapContext, MapEntry, MappableSection, MappableTable, MappedFile,
    MappedSection, Placement, SectionDtype,
};
use super::*;
use crate::datatypes::Value;
use crate::graph::schema::InternedKey;
use crate::graph::storage::disk::id_index::{IdIndexBase, IdIndexStore};
use crate::graph::storage::disk::type_index::{TypeIndexBase, TypeIndexStore};

pub(super) const OWNER: &str = "indexes";

/// Section classes (`MapEntry::kind`) owned by this module.
pub(super) const KIND_TYPE_MEMBERS: u8 = 6;
pub(super) const KIND_ID_KEYS: u8 = 7;
pub(super) const KIND_ID_NODES: u8 = 8;

/// A save records the type and id indexes of a graph with at least this many
/// live nodes. Below it the rebuild is too cheap to be worth file bytes, and
/// a small graph stays a v8 container older readers can open.
pub(crate) const MIN_NODES: usize = 65_536;

/// Node types beyond which a save records nothing: each is a few table rows,
/// and a catalogue this wide is not what the sections are for.
const MAX_TYPES: usize = 1024;

#[cfg(test)]
thread_local! {
    static FLOOR: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Run `body` with the node floor a save applies replaced, so a test can
/// exercise the sections without building a 65,536-node graph.
#[cfg(test)]
pub(crate) fn with_floor<R>(nodes: usize, body: impl FnOnce() -> R) -> R {
    FLOOR.with(|floor| floor.set(Some(nodes)));
    let result = body();
    FLOOR.with(|floor| floor.set(None));
    result
}

fn node_floor() -> usize {
    #[cfg(test)]
    if let Some(floor) = FLOOR.with(|floor| floor.get()) {
        return floor;
    }
    MIN_NODES
}

fn type_section_name(node_type: &str) -> String {
    format!("idx:type:{}", escape(node_type))
}

fn id_section_name(node_type: &str, part: &str) -> String {
    format!("idx:id:{}:{part}", escape(node_type))
}

fn owned(
    name: String,
    kind: u8,
    dtype: SectionDtype,
    count: usize,
    data: Vec<u8>,
) -> MappableSection<'static> {
    MappableSection {
        name,
        owner: OWNER.to_string(),
        kind,
        dtype,
        elem_count: count as u64,
        flags: 0,
        data: std::borrow::Cow::Owned(data),
    }
}

/// The one id spelling a type's index can be written in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum IdKind {
    /// `Value::UniqueId`, what a DataFrame load stores: `u32` keys.
    Unique,
    /// `Value::Int64`: `i64` keys.
    Int64,
}

/// What one pass over a type's nodes collects.
#[derive(Default)]
struct TypeScan {
    members: Vec<u32>,
    /// `(id, node)` while every id seen so far has the same spelling.
    ids: Vec<(i64, u32)>,
    kind: Option<IdKind>,
    /// False once an id of another spelling, or a node without one, is seen.
    uniform: bool,
}

impl TypeScan {
    fn see(&mut self, id: &Value, node: u32) {
        let (kind, value) = match id {
            Value::UniqueId(id) => (IdKind::Unique, i64::from(*id)),
            Value::Int64(id) => (IdKind::Int64, *id),
            _ => {
                self.uniform = false;
                self.ids = Vec::new();
                return;
            }
        };
        if *self.kind.get_or_insert(kind) == kind {
            self.ids.push((value, node));
        } else {
            self.uniform = false;
            self.ids = Vec::new();
        }
    }
}

/// The index sections a save of `graph` writes: none for a small graph or on
/// a big-endian host (the bytes are the format's, not the host's).
pub(super) fn collect_index_sections(
    graph: &DirGraph,
) -> io::Result<Vec<MappableSection<'static>>> {
    let mut out = Vec::new();
    if !cfg!(target_endian = "little") {
        return Ok(out);
    }
    if graph.graph.node_count() >= node_floor() && graph.graph.node_bound() <= u32::MAX as usize {
        push_node_sections(graph, &mut out);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn push_node_sections(graph: &DirGraph, out: &mut Vec<MappableSection<'static>>) {
    let mut scans: FxHashMap<InternedKey, TypeScan> = FxHashMap::default();
    {
        let _guard = graph.graph.begin_query();
        for idx in graph.graph.node_indices() {
            let Some(node) = graph.graph.node_view(idx) else {
                continue;
            };
            let scan = scans.entry(node.node_type()).or_insert_with(|| TypeScan {
                uniform: true,
                ..TypeScan::default()
            });
            let position = idx.index() as u32;
            scan.members.push(position);
            if scan.uniform {
                scan.see(node.id().as_ref(), position);
            }
        }
    }
    if scans.len() > MAX_TYPES {
        return;
    }
    for (type_key, mut scan) in scans {
        let name = graph.interner.resolve(type_key);
        out.push(owned(
            type_section_name(name),
            KIND_TYPE_MEMBERS,
            SectionDtype::U32,
            scan.members.len(),
            scan.members.iter().flat_map(|n| n.to_le_bytes()).collect(),
        ));
        let Some(kind) = scan.kind.filter(|_| scan.uniform && !scan.ids.is_empty()) else {
            continue;
        };
        scan.ids.sort_unstable_by_key(|(id, _)| *id);
        // A repeated id has no single owner, so the index would pick a node
        // the lazy build picks differently; the type is left to the lazy build.
        if scan.ids.windows(2).any(|w| w[0].0 == w[1].0) {
            continue;
        }
        let count = scan.ids.len();
        let (dtype, keys): (SectionDtype, Vec<u8>) = match kind {
            IdKind::Int64 => (
                SectionDtype::I64,
                scan.ids
                    .iter()
                    .flat_map(|(id, _)| id.to_le_bytes())
                    .collect(),
            ),
            IdKind::Unique => (
                SectionDtype::U32,
                scan.ids
                    .iter()
                    .flat_map(|(id, _)| (*id as u32).to_le_bytes())
                    .collect(),
            ),
        };
        out.push(owned(
            id_section_name(name, "keys"),
            KIND_ID_KEYS,
            dtype,
            count,
            keys,
        ));
        out.push(owned(
            id_section_name(name, "nodes"),
            KIND_ID_NODES,
            SectionDtype::U32,
            count,
            scan.ids.iter().flat_map(|(_, n)| n.to_le_bytes()).collect(),
        ));
    }
}

/// Where a load puts one index section.
///
/// Into memory storage nothing reads them, so a file-backed section is left
/// unread in the mapping and a byte-buffer load keeps only the validated
/// range. Into mapped storage they follow `columns`, except that Windows
/// always copies: an open mapping there blocks the rename a save performs.
pub(super) fn place(
    entry: &MapEntry,
    file: Option<&Arc<MappedFile>>,
    policy: MapPolicy,
    into_mapped: bool,
) -> Placement {
    if !into_mapped {
        return if file.is_some() {
            Placement::Mapped
        } else {
            Placement::Deferred
        };
    }
    let ctx = MapContext::host(file.map(|f| &**f), entry.len);
    if !cfg!(windows) && resolve_mapping(policy, 0, &ctx) {
        Placement::Mapped
    } else {
        Placement::Heap
    }
}

/// The index sections of an opened table that a mapped load can serve.
#[derive(Default)]
pub(super) struct IndexSections {
    types: Vec<(String, MappedSection)>,
    ids: Vec<(String, MappedSection, MappedSection)>,
}

impl IndexSections {
    /// Collect the index sections a table holds. A section of another kind or
    /// element type than its name promises, or one missing its partner, is
    /// ignored: an index is derived data, and the loader rebuilds whatever the
    /// file does not serve.
    pub(super) fn from_table(table: &MappableTable) -> Self {
        let partner = |name: String, kind: u8, dtype: SectionDtype| {
            table
                .get(&name)
                .filter(|s| s.owner == OWNER && s.kind == kind && s.dtype == dtype)
                .cloned()
        };
        let mut found = IndexSections::default();
        for section in table.iter().filter(|s| s.owner == OWNER) {
            let name = section.name.as_str();
            match (section.kind, section.dtype) {
                (KIND_TYPE_MEMBERS, SectionDtype::U32) => {
                    if let Some(stem) = name.strip_prefix("idx:type:") {
                        found.types.push((unescape(stem), section.clone()));
                    }
                }
                (KIND_ID_KEYS, SectionDtype::I64 | SectionDtype::U32) => {
                    let stem = name
                        .strip_prefix("idx:id:")
                        .and_then(|rest| rest.strip_suffix(":keys"));
                    if let Some(stem) = stem {
                        let nodes = partner(
                            format!("idx:id:{stem}:nodes"),
                            KIND_ID_NODES,
                            SectionDtype::U32,
                        );
                        if let Some(nodes) = nodes {
                            found.ids.push((unescape(stem), section.clone(), nodes));
                        }
                    }
                }
                _ => {}
            }
        }
        found
    }

    pub(super) fn has_types(&self) -> bool {
        !self.types.is_empty()
    }

    /// Install the type-member and id indexes on `graph`, whose topology was
    /// just decoded and whose type indexes are not yet built. Returns `false`
    /// without touching `graph` when the sections do not describe it (a type
    /// count that disagrees with the topology); the caller then rebuilds.
    pub(super) fn install_node_indexes(&mut self, graph: &mut DirGraph) -> io::Result<bool> {
        let bound = graph.graph.node_bound();
        let total: usize = self
            .types
            .iter()
            .map(|(_, section)| section.as_bytes().len() / 4)
            .sum();
        if total != graph.graph.node_count() {
            return Ok(false);
        }
        let types = TypeIndexBase::from_sections(std::mem::take(&mut self.types), bound)?;
        let ids: Vec<_> = std::mem::take(&mut self.ids)
            .into_iter()
            .filter(|(name, keys, _)| {
                types
                    .slice_for(name)
                    .is_some_and(|members| members.len() / 4 == keys.elem_count as usize)
            })
            .collect();
        let id_base = IdIndexBase::from_sections(ids, bound)?;
        graph.type_indices = TypeIndexStore::from_base(types);
        graph.id_indices = IdIndexStore::from_base(id_base);
        Ok(true)
    }
}
