//! The Int64Sorted variant: 12 bytes per id, searched in the mapping, with a
//! delta layered over it by the first write and merged into the next file.
use super::*;
use crate::graph::storage::disk::type_index::TypeIndexStore;
use std::collections::BTreeMap;

const BIG: i64 = 3_100_000_000_000;
const NAME: &str = "Pand";

fn interner() -> StringInterner {
    let mut interner = StringInterner::new();
    interner.get_or_intern(NAME);
    interner
}

fn id_of(i: usize) -> i64 {
    BIG + i as i64 * 7
}

fn general(ids: impl IntoIterator<Item = (i64, usize)>) -> TypeIdIndex {
    TypeIdIndex::General(
        ids.into_iter()
            .map(|(id, node)| (Value::Int64(id), NodeIndex::new(node)))
            .collect(),
    )
}

fn store_of(index: TypeIdIndex) -> IdIndexStore {
    let mut store = IdIndexStore::default();
    store.replace_with(HashMap::from([(NAME.to_string(), index)]));
    store
}

fn members(count: usize) -> TypeIndexStore {
    let mut members = TypeIndexStore::default();
    for node in 0..count {
        members.push_to_type(NAME, NodeIndex::new(node));
    }
    members
}

/// `(variant, num_entries, payload_len)` per directory entry, in file order.
fn directory(dir: &Path) -> Vec<(u8, u64, u64)> {
    let bytes = std::fs::read(dir.join("id_indices.bin")).unwrap();
    let count = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    (0..count)
        .map(|i| {
            let at = HEADER_BYTES + i * DIR_ENTRY_BYTES;
            (
                bytes[at + 8],
                u64::from_le_bytes(bytes[at + 16..at + 24].try_into().unwrap()),
                u64::from_le_bytes(bytes[at + 32..at + 40].try_into().unwrap()),
            )
        })
        .collect()
}

fn write(dir: &Path, store: &IdIndexStore, members: &TypeIndexStore) {
    write_id_indices_bin(dir, store, members, &interner()).unwrap();
}

fn load(dir: &Path) -> IdIndexBase {
    IdIndexBase::load_from(dir, &interner()).unwrap().unwrap()
}

fn all_pairs(base: &IdIndexBase) -> BTreeMap<i64, u32> {
    let (keys, nodes) = base.int64_parts(NAME).expect("an Int64Sorted entry");
    (0..keys.len() / 8)
        .map(|i| {
            (
                read_le_i64(keys, i).unwrap(),
                read_le_u32(nodes, i).unwrap(),
            )
        })
        .collect()
}

#[test]
fn an_all_int64_index_is_written_as_the_sorted_variant_at_twelve_bytes_per_id() {
    let dir = tempfile::tempdir().unwrap();
    // Inserted out of order: the file is sorted regardless.
    let index = general((0..500).rev().map(|i| (id_of(i), i)));
    write(dir.path(), &store_of(index), &members(500));

    assert_eq!(
        directory(dir.path()),
        [(VARIANT_INT64, 500, 500 * INT64_ENTRY_BYTES as u64)]
    );
    let base = load(dir.path());
    let pairs = all_pairs(&base);
    assert_eq!(pairs.len(), 500);
    assert!(pairs.keys().zip(pairs.keys().skip(1)).all(|(a, b)| a < b));
    assert_eq!(pairs[&id_of(123)], 123);
    assert_eq!(
        base.lookup(NAME, &Value::Int64(id_of(499))),
        Some(NodeIndex::new(499))
    );
    assert_eq!(base.lookup(NAME, &Value::Int64(id_of(500))), None);
    assert_eq!(base.entry_len(NAME), Some(500));
}

/// Every kind of query answers as the in-memory `General` index does.
#[test]
fn a_mapped_int64_entry_answers_every_query_as_the_general_index_does() {
    let dir = tempfile::tempdir().unwrap();
    let ids = [
        i64::MIN,
        -1,
        0,
        1,
        41,
        u32::MAX as i64,
        u32::MAX as i64 + 1,
        BIG,
        BIG + 7,
        i64::MAX,
    ];
    let index = general(ids.iter().enumerate().map(|(node, id)| (*id, node)));
    write(dir.path(), &store_of(index.clone()), &members(ids.len()));
    let base = load(dir.path());
    assert_eq!(directory(dir.path())[0].0, VARIANT_INT64);

    let mut queries: Vec<Value> = Vec::new();
    for id in ids
        .iter()
        .copied()
        .chain([2, 40, -2, BIG + 1, i64::MAX - 1])
    {
        queries.push(Value::Int64(id));
        queries.push(Value::Float64(id as f64));
        queries.push(Value::String(id.to_string()));
        if let Ok(id) = u32::try_from(id) {
            queries.push(Value::UniqueId(id));
        }
    }
    queries.extend([
        Value::Float64(1.5),
        Value::Float64(f64::NAN),
        Value::Float64(f64::INFINITY),
        Value::Float64(9.3e18),
        Value::Float64(-9.3e18),
        Value::Null,
        Value::Boolean(true),
    ]);
    for query in &queries {
        assert_eq!(
            base.lookup(NAME, query),
            index.get(query),
            "lookup {query:?}"
        );
        assert_eq!(
            base.lookup_exact(NAME, query),
            index.get_exact(query),
            "exact lookup {query:?}"
        );
    }
}

/// A file whose Int64Sorted entry is not what the reader binary-searches is
/// refused at load, never answered wrongly.
#[test]
fn a_malformed_int64_payload_is_refused_at_load() {
    let interner = interner();
    let key = InternedKey::from_str(NAME).as_u64();
    let fixture = |keys: &[i64], nodes: &[u32], declared_len: u64| {
        let data_offset = HEADER_BYTES + DIR_ENTRY_BYTES;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(HEADER_BYTES as u64).to_le_bytes());
        bytes.extend_from_slice(&(data_offset as u64).to_le_bytes());
        bytes.extend_from_slice(&key.to_le_bytes());
        bytes.push(VARIANT_INT64);
        bytes.extend_from_slice(&[0; 7]);
        bytes.extend_from_slice(&(keys.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&(data_offset as u64).to_le_bytes());
        bytes.extend_from_slice(&declared_len.to_le_bytes());
        bytes.extend_from_slice(&[0; 8]);
        keys.iter()
            .for_each(|k| bytes.extend_from_slice(&k.to_le_bytes()));
        nodes
            .iter()
            .for_each(|n| bytes.extend_from_slice(&n.to_le_bytes()));
        bytes
    };
    let refused = |bytes: Vec<u8>, why: &str| {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("id_indices.bin"), bytes).unwrap();
        let error = IdIndexBase::load_from(dir.path(), &interner)
            .err()
            .unwrap_or_else(|| panic!("{why}: loaded"));
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{why}");
    };
    let good = fixture(&[BIG, BIG + 7], &[1, 2], 24);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("id_indices.bin"), &good).unwrap();
    assert!(IdIndexBase::load_from(dir.path(), &interner).is_ok());

    refused(fixture(&[BIG + 7, BIG], &[1, 2], 24), "unsorted keys");
    refused(fixture(&[BIG, BIG], &[1, 2], 24), "duplicate keys");
    refused(
        fixture(&[BIG, BIG + 7], &[1, 2], 23),
        "declared length short",
    );
    refused(fixture(&[BIG, BIG + 7], &[1], 24), "missing node");
    let mut truncated = good.clone();
    truncated.truncate(truncated.len() - 4);
    refused(truncated, "truncated file");
}

#[test]
fn a_write_delta_over_the_mapping_merges_add_overwrite_and_delete_into_the_next_file() {
    let dir = tempfile::tempdir().unwrap();
    let count = 1_000;
    write(
        dir.path(),
        &store_of(general((0..count).map(|i| (id_of(i), i)))),
        &members(count),
    );
    let mut model: BTreeMap<i64, u32> = (0..count).map(|i| (id_of(i), i as u32)).collect();

    let mut store = IdIndexStore::from_base(load(dir.path()));
    let built = full_maps_built();
    {
        let entry = store.entry_or_default(NAME.to_string());
        assert!(matches!(entry, TypeEntry::OverBase { .. }));
        // Below the smallest key, above the largest, and between two.
        for (id, node) in [
            (BIG - 5, 5_000),
            (id_of(count) + 100, 5_001),
            (id_of(10) + 3, 5_002),
        ] {
            entry.insert(Value::Int64(id), NodeIndex::new(node as usize));
            model.insert(id, node);
        }
        // An id moves to another node.
        entry.insert(Value::Int64(id_of(20)), NodeIndex::new(6_000));
        model.insert(id_of(20), 6_000);
        // A base id is deleted, a delta-only id is deleted, a deleted id returns.
        assert!(entry.remove_matching(&Value::Int64(id_of(30)), NodeIndex::new(30)));
        model.remove(&id_of(30));
        assert!(entry.remove_matching(&Value::Int64(id_of(10) + 3), NodeIndex::new(5_002)));
        model.remove(&(id_of(10) + 3));
        assert!(entry.remove_matching(&Value::Int64(id_of(40)), NodeIndex::new(40)));
        entry.insert(Value::Int64(id_of(40)), NodeIndex::new(7_000));
        model.insert(id_of(40), 7_000);
        // The wrong node leaves an id alone.
        assert!(!entry.remove_matching(&Value::Int64(id_of(50)), NodeIndex::new(51)));
    }
    assert_eq!(full_maps_built(), built, "a first write built a full map");

    let entry = store.entry_or_default(NAME.to_string());
    assert_eq!(entry.len(), model.len());
    for (id, node) in &model {
        assert_eq!(
            entry.get(&Value::Int64(*id)),
            Some(NodeIndex::new(*node as usize))
        );
    }
    assert_eq!(entry.get(&Value::Int64(id_of(30))), None);
    assert_eq!(entry.get(&Value::Int64(id_of(10) + 3)), None);

    let out = tempfile::tempdir().unwrap();
    write(out.path(), &store, &members(model.len()));
    assert_eq!(full_maps_built(), built, "the save built a full map");
    let reloaded = load(out.path());
    assert_eq!(all_pairs(&reloaded), model);
    assert_eq!(
        directory(out.path()),
        [(VARIANT_INT64, model.len() as u64, model.len() as u64 * 12)]
    );
}

#[test]
fn a_write_delta_that_deletes_every_id_writes_an_empty_entry() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &store_of(general((0..10).map(|i| (id_of(i), i)))),
        &members(10),
    );
    let mut store = IdIndexStore::from_base(load(dir.path()));
    let entry = store.entry_or_default(NAME.to_string());
    for i in 0..10 {
        assert!(entry.remove_matching(&Value::Int64(id_of(i)), NodeIndex::new(i)));
    }
    assert_eq!(entry.len(), 0);
    let out = tempfile::tempdir().unwrap();
    write(out.path(), &store, &members(0));
    assert_eq!(directory(out.path()), [(VARIANT_INT64, 0, 0)]);
    assert!(load(out.path())
        .lookup(NAME, &Value::Int64(id_of(3)))
        .is_none());
}

#[test]
fn an_unchanged_entry_is_copied_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &store_of(general((0..300).map(|i| (id_of(i), i)))),
        &members(300),
    );
    let original = std::fs::read(dir.path().join("id_indices.bin")).unwrap();
    let store = IdIndexStore::from_base(load(dir.path()));
    let out = tempfile::tempdir().unwrap();
    write(out.path(), &store, &members(300));
    assert_eq!(
        std::fs::read(out.path().join("id_indices.bin")).unwrap(),
        original
    );

    // A layered-then-untouched delta merges to the same bytes as well.
    let mut store = IdIndexStore::from_base(load(dir.path()));
    store.entry_or_default(NAME.to_string());
    let merged = tempfile::tempdir().unwrap();
    write(merged.path(), &store, &members(300));
    assert_eq!(
        std::fs::read(merged.path().join("id_indices.bin")).unwrap(),
        original
    );
}

/// A type whose ids repeat is never written: the reload rebuilds it from the
/// type's node order. The rule holds for every representation.
#[test]
fn a_type_whose_ids_repeat_is_not_written_in_any_representation() {
    let entries = |dir: &Path| directory(dir).len();

    // An owned index with fewer ids than the type has nodes.
    let dir = tempfile::tempdir().unwrap();
    let owned = store_of(general((0..10).map(|i| (id_of(i), i))));
    write(dir.path(), &owned, &members(11));
    assert_eq!(entries(dir.path()), 0);
    write(dir.path(), &owned, &members(10));
    assert_eq!(entries(dir.path()), 1);

    // A base entry the type has outgrown.
    let mut store = IdIndexStore::from_base(load(dir.path()));
    let out = tempfile::tempdir().unwrap();
    write(out.path(), &store, &members(12));
    assert_eq!(entries(out.path()), 0);

    // A delta whose merged count falls short of the type's nodes.
    store
        .entry_or_default(NAME.to_string())
        .insert(Value::Int64(id_of(99)), NodeIndex::new(99));
    write(out.path(), &store, &members(12));
    assert_eq!(entries(out.path()), 0);
    write(out.path(), &store, &members(11));
    assert_eq!(entries(out.path()), 1);
}

#[test]
fn an_id_that_is_not_an_int64_moves_the_type_off_the_sorted_layout() {
    let dir = tempfile::tempdir().unwrap();
    // Small ids, so `UniqueId(1005)` is another spelling of the base's `Int64(1005)`.
    write(
        dir.path(),
        &store_of(general((0..50).map(|i| (1_000 + i as i64, i)))),
        &members(50),
    );
    let mut store = IdIndexStore::from_base(load(dir.path()));
    let entry = store.entry_or_default(NAME.to_string());
    entry.insert(Value::String("legacy-7".into()), NodeIndex::new(50));
    entry.insert(Value::UniqueId(1_005), NodeIndex::new(51));
    assert!(matches!(entry, TypeEntry::OverBase { .. }));
    assert_eq!(entry.get(&Value::Int64(1_005)), Some(NodeIndex::new(51)));

    let out = tempfile::tempdir().unwrap();
    // 50 base ids and one string: the second spelling is not a new id.
    write(out.path(), &store, &members(51));
    assert_eq!(directory(out.path())[0].0, VARIANT_GENERAL);
    let reloaded = load(out.path());
    assert_eq!(
        reloaded.lookup(NAME, &Value::String("legacy-7".into())),
        Some(NodeIndex::new(50))
    );
    assert_eq!(
        reloaded.lookup(NAME, &Value::Int64(1_003)),
        Some(NodeIndex::new(3))
    );
    for spelling in [
        Value::UniqueId(1_005),
        Value::Int64(1_005),
        Value::Float64(1_005.0),
    ] {
        assert_eq!(
            reloaded.lookup(NAME, &spelling),
            Some(NodeIndex::new(51)),
            "{spelling:?}: the node inserted last answers every spelling"
        );
    }
}

#[test]
fn a_lookup_a_first_write_a_length_and_a_save_never_build_a_full_map() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &store_of(general((0..2_000).map(|i| (id_of(i), i)))),
        &members(2_000),
    );
    let built = full_maps_built();
    let mut store = IdIndexStore::from_base(load(dir.path()));
    assert_eq!(
        store.lookup(NAME, &Value::Int64(id_of(77))),
        Some(NodeIndex::new(77))
    );
    assert_eq!(store.lookup(NAME, &Value::UniqueId(3)), None);
    assert!(store.contains_key(NAME));
    let entry = store.entry_or_default(NAME.to_string());
    entry.insert(Value::Int64(id_of(5_000)), NodeIndex::new(5_000));
    assert_eq!(entry.len(), 2_001);
    assert_eq!(
        store.lookup(NAME, &Value::Int64(id_of(5_000))),
        Some(NodeIndex::new(5_000))
    );
    let out = tempfile::tempdir().unwrap();
    write(out.path(), &store, &members(2_001));
    assert_eq!(full_maps_built(), built);

    // Where a full map is the honest cost, the counter moves.
    let base = load(dir.path());
    base.materialize(NAME).unwrap();
    assert_eq!(full_maps_built(), built + 1);
}

#[test]
fn compacting_a_layer_over_a_delta_keeps_it_a_delta() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &store_of(general((0..100).map(|i| (id_of(i), i)))),
        &members(100),
    );
    let mut store = IdIndexStore::from_base(load(dir.path()));
    store
        .entry_or_default(NAME.to_string())
        .insert(Value::Int64(id_of(500)), NodeIndex::new(500));
    let built = full_maps_built();

    // A fork shares the entry; dropping it lets the parent fold its layer back.
    let fork = store.clone();
    store
        .entry_or_default(NAME.to_string())
        .insert(Value::Int64(id_of(501)), NodeIndex::new(501));
    drop(fork);
    store.try_compact();
    let entry = store.entry_or_default(NAME.to_string());
    assert!(
        matches!(entry, TypeEntry::OverBase { .. }),
        "the compacted entry is a delta over the mapping again, not a heap copy"
    );
    assert_eq!(entry.len(), 102);
    assert_eq!(
        entry.get(&Value::Int64(id_of(501))),
        Some(NodeIndex::new(501))
    );
    assert_eq!(
        entry.get(&Value::Int64(id_of(99))),
        Some(NodeIndex::new(99))
    );
    assert_eq!(full_maps_built(), built);
}

/// A fork (a transaction, a held view) wraps the delta in a shared layer; a save
/// taken while it is alive still merges into the next file without a full map.
#[test]
fn a_save_while_a_fork_shares_the_delta_still_merges_in_place() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &store_of(general((0..400).map(|i| (id_of(i), i)))),
        &members(400),
    );
    let mut store = IdIndexStore::from_base(load(dir.path()));
    let mut model: BTreeMap<i64, u32> = (0..400).map(|i| (id_of(i), i as u32)).collect();
    let entry = store.entry_or_default(NAME.to_string());
    entry.insert(Value::Int64(id_of(1_000)), NodeIndex::new(1_000));
    model.insert(id_of(1_000), 1_000);

    let fork = store.clone();
    let entry = store.entry_or_default(NAME.to_string());
    assert!(matches!(entry, TypeEntry::Layered { .. }));
    entry.insert(Value::Int64(id_of(1_001)), NodeIndex::new(1_001));
    model.insert(id_of(1_001), 1_001);
    assert!(entry.remove_matching(&Value::Int64(id_of(7)), NodeIndex::new(7)));
    model.remove(&id_of(7));
    assert!(entry.remove_matching(&Value::Int64(id_of(1_000)), NodeIndex::new(1_000)));
    model.remove(&id_of(1_000));

    let built = full_maps_built();
    let out = tempfile::tempdir().unwrap();
    write(out.path(), &store, &members(model.len()));
    assert_eq!(
        full_maps_built(),
        built,
        "the layered save built a full map"
    );
    assert_eq!(all_pairs(&load(out.path())), model);
    drop(fork);
}

/// The published file replaces every overlay entry it covers; a type it did not
/// write keeps its heap entry; and a file that disagrees changes nothing.
#[test]
fn rebasing_onto_the_published_file_drops_the_covered_entries_only() {
    let mut interner = interner();
    interner.get_or_intern("Skipped");
    let mut store = IdIndexStore::default();
    store.replace_with(HashMap::from([
        (NAME.to_string(), general((0..50).map(|i| (id_of(i), i)))),
        (
            "Skipped".to_string(),
            general((0..5).map(|i| (id_of(i), i))),
        ),
    ]));
    let mut nodes = members(50);
    for node in 0..6 {
        nodes.push_to_type("Skipped", NodeIndex::new(node));
    }
    let dir = tempfile::tempdir().unwrap();
    write_id_indices_bin(dir.path(), &store, &nodes, &interner).unwrap();
    assert_eq!(
        directory(dir.path()).len(),
        1,
        "the repeating type is not written"
    );

    let base = IdIndexBase::load_from(dir.path(), &interner)
        .unwrap()
        .unwrap();
    store.rebase_onto(base).unwrap();
    assert_eq!(store.overlay_len(NAME), None, "served from the file now");
    assert_eq!(store.overlay_len("Skipped"), Some(5), "kept its heap entry");
    assert_eq!(
        store.lookup(NAME, &Value::Int64(id_of(9))),
        Some(NodeIndex::new(9))
    );
    assert_eq!(
        store.lookup("Skipped", &Value::Int64(id_of(4))),
        Some(NodeIndex::new(4))
    );

    // A live entry the file disagrees with is left in place.
    let mut divergent = IdIndexStore::default();
    divergent.replace_with(HashMap::from([(
        NAME.to_string(),
        general((0..49).map(|i| (id_of(i), i))),
    )]));
    let base = IdIndexBase::load_from(dir.path(), &interner)
        .unwrap()
        .unwrap();
    assert!(divergent.rebase_onto(base).is_err());
    assert_eq!(divergent.overlay_len(NAME), Some(49));
}
