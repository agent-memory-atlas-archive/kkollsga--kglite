//! `id_indices.bin` reader, writer and store: validation, round trips and
//! the version-2 compatibility fixtures.
use super::*;
use crate::graph::storage::disk::temp_owner::{TempGraphDir, TrackedOwner};
use crate::graph::storage::disk::type_index::TypeIndexStore;

fn integer_fixture(type_key: u64, pairs: &[(u32, u32)]) -> Vec<u8> {
    let data_offset = HEADER_BYTES + DIR_ENTRY_BYTES;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&(HEADER_BYTES as u64).to_le_bytes());
    bytes.extend_from_slice(&(data_offset as u64).to_le_bytes());
    bytes.extend_from_slice(&type_key.to_le_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&[0; 7]);
    bytes.extend_from_slice(&(pairs.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&(data_offset as u64).to_le_bytes());
    bytes.extend_from_slice(&((pairs.len() * 8) as u64).to_le_bytes());
    bytes.extend_from_slice(&[0; 8]);
    for (key, _) in pairs {
        bytes.extend_from_slice(&key.to_le_bytes());
    }
    for (_, node) in pairs {
        bytes.extend_from_slice(&node.to_le_bytes());
    }
    bytes
}

/// A loaded [`IdIndexBase`] together with the temp directory its `mmap`
/// points into. Field order is the contract: `base` drops before `temp`,
/// and `temp`'s guard asserts it.
///
/// The previous helper returned the base alone, so the `TempDir` local
/// was dropped the moment `load` returned and every assertion below ran
/// against an unlinked inode — valid on Unix, and therefore silent.
struct LoadedIndex {
    base: TrackedOwner<IdIndexBase>,
    /// Held only for its `Drop`: it asserts `base` above is gone.
    _temp: TempGraphDir,
}

impl LoadedIndex {
    fn base(&self) -> &IdIndexBase {
        &self.base
    }
}

fn load(bytes: &[u8], interner: &StringInterner) -> std::io::Result<Option<LoadedIndex>> {
    let temp = TempGraphDir::new();
    std::fs::write(temp.path().join("id_indices.bin"), bytes).unwrap();
    let Some(base) = IdIndexBase::load_from(temp.path(), interner)? else {
        return Ok(None);
    };
    let base = temp.own("IdIndexBase", base);
    Ok(Some(LoadedIndex { base, _temp: temp }))
}

fn assert_invalid(bytes: &[u8], interner: &StringInterner) {
    let outcome = std::panic::catch_unwind(|| load(bytes, interner));
    match outcome.expect("invalid index must not panic") {
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::InvalidData),
        Ok(_) => panic!("invalid index loaded successfully"),
    }
}

#[test]
fn integer_fixture_reads_canonical_little_endian_bytes() {
    let mut interner = StringInterner::new();
    let key = interner.get_or_intern("Person").as_u64();
    let loaded = load(&integer_fixture(key, &[(7, 70), (42, 420)]), &interner)
        .unwrap()
        .unwrap();
    let base = loaded.base();
    assert_eq!(
        base.lookup("Person", &Value::UniqueId(7)),
        Some(NodeIndex::new(70))
    );
    assert_eq!(
        base.lookup("Person", &Value::UniqueId(42)),
        Some(NodeIndex::new(420))
    );
}

#[test]
fn rejects_invalid_header_directory_and_variant() {
    let mut interner = StringInterner::new();
    let key = interner.get_or_intern("Person").as_u64();
    let valid = integer_fixture(key, &[(7, 70)]);

    let mut huge_count = valid.clone();
    huge_count[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_invalid(&huge_count, &interner);
    let mut bad_dir = valid.clone();
    bad_dir[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_invalid(&bad_dir, &interner);
    let mut bad_data = valid.clone();
    bad_data[24..32].copy_from_slice(&33u64.to_le_bytes());
    assert_invalid(&bad_data, &interner);
    let mut bad_variant = valid.clone();
    bad_variant[40] = 2;
    assert_invalid(&bad_variant, &interner);
}

#[test]
fn rejects_bad_counts_ranges_and_integer_ordering() {
    let mut interner = StringInterner::new();
    let key = interner.get_or_intern("Person").as_u64();
    let valid = integer_fixture(key, &[(7, 70), (42, 420)]);

    let mut too_many = valid.clone();
    too_many[48..56].copy_from_slice(&(u32::MAX as u64 + 1).to_le_bytes());
    assert_invalid(&too_many, &interner);
    let mut past_eof = valid.clone();
    past_eof[56..64].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_invalid(&past_eof, &interner);
    let mut wrong_len = valid.clone();
    wrong_len[64..72].copy_from_slice(&15u64.to_le_bytes());
    assert_invalid(&wrong_len, &interner);
    assert_invalid(&integer_fixture(key, &[(42, 1), (7, 2)]), &interner);
    assert_invalid(&integer_fixture(key, &[(7, 1), (7, 2)]), &interner);
}

#[test]
fn rejects_malformed_general_postcard_during_load() {
    let mut interner = StringInterner::new();
    let key = interner.get_or_intern("StringIds").as_u64();
    let mut bytes = integer_fixture(key, &[(1, 1)]);
    bytes[40] = 1;
    bytes[64..72].copy_from_slice(&16u64.to_le_bytes());
    bytes.truncate(HEADER_BYTES + DIR_ENTRY_BYTES);
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.extend_from_slice(&[0xff; 8]);
    assert_invalid(&bytes, &interner);
}

#[test]
fn writer_round_trip_accepts_unaligned_integer_payload_after_general() {
    let temp = tempfile::tempdir().unwrap();
    let mut interner = StringInterner::new();
    let candidates = ["Alpha", "Beta"];
    for name in candidates {
        interner.get_or_intern(name);
    }
    let mut ordered = candidates;
    ordered.sort_by_key(|name| InternedKey::from_str(name).as_u64());
    let general_name = ordered[0];
    let integer_name = ordered[1];
    let general = TypeIdIndex::General(FxHashMap::from_iter([(
        Value::String("x".into()),
        NodeIndex::new(3),
    )]));
    let integer = TypeIdIndex::Integer(FxHashMap::from_iter([(7, NodeIndex::new(4))]));
    let mut store = IdIndexStore::default();
    store.replace_with(HashMap::from([
        (general_name.to_string(), general),
        (integer_name.to_string(), integer),
    ]));
    write_id_indices_bin(temp.path(), &store, &TypeIndexStore::default(), &interner).unwrap();

    let raw = std::fs::read(temp.path().join("id_indices.bin")).unwrap();
    let second_payload_off = u64::from_le_bytes(
        raw[HEADER_BYTES + DIR_ENTRY_BYTES + 24..HEADER_BYTES + DIR_ENTRY_BYTES + 32]
            .try_into()
            .unwrap(),
    );
    assert_ne!(
        second_payload_off % 4,
        0,
        "fixture must exercise an unaligned integer payload"
    );

    let base = IdIndexBase::load_from(temp.path(), &interner)
        .unwrap()
        .unwrap();
    assert_eq!(
        base.lookup(general_name, &Value::String("x".into())),
        Some(NodeIndex::new(3))
    );
    assert_eq!(
        base.lookup(integer_name, &Value::UniqueId(7)),
        Some(NodeIndex::new(4))
    );
}

/// A persisted `General` index answers every numeric spelling of an id
/// as the in-memory index does: a float id is found by an integer and a
/// `UniqueId` query, and an integer id by a float one.
#[test]
fn a_persisted_general_index_coerces_like_the_in_memory_one() {
    let temp = tempfile::tempdir().unwrap();
    let mut interner = StringInterner::new();
    interner.get_or_intern("G");
    let index = TypeIdIndex::General(FxHashMap::from_iter([
        (Value::Float64(1.0), NodeIndex::new(0)),
        (Value::Int64(2), NodeIndex::new(1)),
        (Value::String("x".into()), NodeIndex::new(2)),
    ]));
    let mut store = IdIndexStore::default();
    store.replace_with(HashMap::from([("G".to_string(), index.clone())]));
    write_id_indices_bin(temp.path(), &store, &TypeIndexStore::default(), &interner).unwrap();
    let base = IdIndexBase::load_from(temp.path(), &interner)
        .unwrap()
        .unwrap();
    for query in [
        Value::Float64(1.0),
        Value::Int64(1),
        Value::UniqueId(1),
        Value::Float64(2.0),
        Value::Int64(2),
        Value::UniqueId(2),
        Value::String("x".into()),
        Value::String("1".into()),
        Value::Float64(1.5),
    ] {
        assert_eq!(base.lookup("G", &query), index.get(&query), "{query:?}");
    }
    assert_eq!(base.lookup("G", &Value::Int64(1)), Some(NodeIndex::new(0)));
}

/// An index spelling one id twice (built through 0.18.1) is healed when
/// read back, and never written: its ids repeat although it has as many
/// entries as the type has nodes.
#[test]
fn a_general_index_spelling_one_id_twice_is_healed_and_not_written() {
    let temp = tempfile::tempdir().unwrap();
    let mut interner = StringInterner::new();
    interner.get_or_intern("M");
    let two_spellings = TypeIdIndex::General(FxHashMap::from_iter([
        (Value::UniqueId(1), NodeIndex::new(0)),
        (Value::Int64(1), NodeIndex::new(1)),
        (Value::Int64(2), NodeIndex::new(2)),
    ]));
    let mut store = IdIndexStore::default();
    store.replace_with(HashMap::from([("M".to_string(), two_spellings)]));

    write_id_indices_bin(temp.path(), &store, &TypeIndexStore::default(), &interner).unwrap();
    let base = IdIndexBase::load_from(temp.path(), &interner)
        .unwrap()
        .unwrap();
    for query in [Value::UniqueId(1), Value::Int64(1), Value::Float64(1.0)] {
        assert_eq!(
            base.lookup("M", &query),
            Some(NodeIndex::new(1)),
            "{query:?}"
        );
    }
    assert_eq!(base.entry_len("M"), Some(2));
    assert_eq!(base.materialize("M").unwrap().len(), 2);
    // The base memory-maps the file; Windows refuses a rewrite while a
    // mapping is open (os error 1224), so release it before writing.
    drop(base);

    let mut members = TypeIndexStore::default();
    for index in 0..3 {
        members.push_to_type("M", NodeIndex::new(index));
    }
    write_id_indices_bin(temp.path(), &store, &members, &interner).unwrap();
    let base = IdIndexBase::load_from(temp.path(), &interner)
        .unwrap()
        .unwrap();
    assert!(!base.contains("M"), "a repeating index must not be written");
}

/// A directory saved before the writer resolved its keys carries an empty
/// index under a type key the interner sidecar never received. Both
/// variants of that entry are recovered rather than failing the load; a
/// populated entry under an unresolvable key still fails (asserted in
/// `rejects_unsupported_unresolved_and_trailing_data`).
#[test]
fn an_empty_entry_with_an_unresolved_type_key_is_recovered() {
    let mut interner = StringInterner::new();
    interner.get_or_intern("Person");
    let stale = InternedKey::from_str("NeverInterned").as_u64();

    let loaded = load(&integer_fixture(stale, &[]), &interner)
        .expect("a stale empty entry must not fail the load")
        .unwrap();
    assert!(!loaded.base().contains("NeverInterned"));

    // The General variant is what a type with no rows actually produced:
    // `num_entries` is 0, but the Postcard payload of an empty map is not.
    let blob = serde_codec::encode_versioned(
        serde_codec::CURRENT_CODEC,
        &HashMap::<Value, NodeIndex>::new(),
        MAX_GENERAL_INDEX_DECODE_BYTES,
    )
    .unwrap();
    let mut bytes = integer_fixture(stale, &[]);
    bytes[40] = 1;
    bytes[64..72].copy_from_slice(&(blob.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&blob);
    let loaded = load(&bytes, &interner)
        .expect("a stale empty General entry must not fail the load")
        .unwrap();
    assert!(!loaded.base().contains("NeverInterned"));
}

/// The writer resolves names against the interner it is handed, so it can
/// never emit a directory key that the matching `interner.bin.zst` fails
/// to resolve. An unregistered name is dropped when its index is empty
/// (pure cache, rebuilt on demand) and fails the save when it is not.
#[test]
fn writer_never_emits_a_key_the_interner_cannot_resolve() {
    let temp = tempfile::tempdir().unwrap();
    let mut interner = StringInterner::new();
    interner.get_or_intern("Known");

    let mut store = IdIndexStore::default();
    store.replace_with(HashMap::from([
        (
            "Known".to_string(),
            TypeIdIndex::Integer(FxHashMap::from_iter([(7, NodeIndex::new(1))])),
        ),
        // Never interned: an id index cached for a type with no rows.
        ("Unregistered".to_string(), TypeIdIndex::default()),
    ]));
    write_id_indices_bin(temp.path(), &store, &TypeIndexStore::default(), &interner).unwrap();

    // Asserted on the bytes, not through the loader: the loader also
    // recovers such an entry (directories written before this fix carry
    // them), so a round trip alone would not pin the writer.
    let raw = std::fs::read(temp.path().join("id_indices.bin")).unwrap();
    assert_eq!(
        u32::from_le_bytes(raw[12..16].try_into().unwrap()),
        1,
        "the unregistered name must not reach the directory at all"
    );

    let base = IdIndexBase::load_from(temp.path(), &interner)
        .expect("the written directory must load")
        .unwrap();
    assert_eq!(
        base.lookup("Known", &Value::UniqueId(7)),
        Some(NodeIndex::new(1))
    );
    assert!(!base.contains("Unregistered"));

    store.insert(
        "Unregistered".to_string(),
        TypeIdIndex::Integer(FxHashMap::from_iter([(1, NodeIndex::new(0))])),
    );
    let error = write_id_indices_bin(temp.path(), &store, &TypeIndexStore::default(), &interner)
        .unwrap_err();
    assert!(error.contains("Unregistered"), "{error}");
    assert!(error.contains("cannot be read back"), "{error}");
}

/// A version-2 file (what 0.19.0 wrote) still opens; the writer emits 3.
#[test]
fn version_2_is_still_read_and_version_3_is_written() {
    let mut interner = StringInterner::new();
    let key = interner.get_or_intern("Person").as_u64();
    let mut bytes = integer_fixture(key, &[(7, 70)]);
    assert_eq!(
        &bytes[8..12],
        &VERSION.to_le_bytes(),
        "fixtures carry the current version"
    );
    assert_eq!(VERSION, 3, "the version that may carry variant 2 is 3");

    bytes[8..12].copy_from_slice(&VERSION_2.to_le_bytes());
    let loaded = load(&bytes, &interner).unwrap().unwrap();
    assert_eq!(
        loaded.base().lookup("Person", &Value::UniqueId(7)),
        Some(NodeIndex::new(70))
    );

    bytes[8..12].copy_from_slice(&(VERSION + 1).to_le_bytes());
    assert_invalid(&bytes, &interner);
}

#[test]
fn rejects_unsupported_unresolved_and_trailing_data() {
    let mut interner = StringInterner::new();
    let key = interner.get_or_intern("Person").as_u64();
    let valid = integer_fixture(key, &[(7, 70)]);
    let mut version = valid.clone();
    version[8..12].copy_from_slice(&(VERSION + 1).to_le_bytes());
    assert_invalid(&version, &interner);
    let mut trailing = valid.clone();
    trailing.push(0);
    assert_invalid(&trailing, &interner);
    assert_invalid(&integer_fixture(key.wrapping_add(1), &[(7, 70)]), &interner);
}
