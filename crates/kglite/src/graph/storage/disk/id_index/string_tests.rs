//! String-keyed id sections: lookups served from the section bytes, the checks
//! a damaged-but-checksummed section must fail, and a delta layered over one.
use super::*;
use crate::graph::io::file::mappable::{MappedSection, SectionDtype};
use crate::graph::storage::disk::type_index::TypeIndexStore;

const KIND_STRINGS: u8 = 19;
const KIND_NODES: u8 = 8;

fn sorted(ids: &[&str]) -> Vec<(String, u32)> {
    let mut pairs: Vec<(String, u32)> = ids
        .iter()
        .enumerate()
        .map(|(node, id)| (id.to_string(), node as u32 + 10))
        .collect();
    pairs.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    pairs
}

fn encode(pairs: &[(String, u32)]) -> (Vec<u8>, Vec<u8>) {
    string_keys::encode(pairs.iter().map(|(id, node)| (id.as_bytes(), *node)))
}

fn sections(keys: Vec<u8>, nodes: Vec<u8>) -> (MappedSection, MappedSection) {
    let count = (nodes.len() / 4) as u64;
    (
        MappedSection::heap_for_test(
            "idx:id:T:keys",
            KIND_STRINGS,
            SectionDtype::U8,
            keys.len() as u64,
            keys,
        ),
        MappedSection::heap_for_test(
            "idx:id:T:nodes",
            KIND_NODES,
            SectionDtype::U32,
            count,
            nodes,
        ),
    )
}

fn base_of(pairs: &[(String, u32)], bound: usize) -> std::io::Result<IdIndexBase> {
    let (keys, nodes) = encode(pairs);
    let (k, n) = sections(keys, nodes);
    IdIndexBase::from_sections(vec![("T".to_string(), k, n)], bound)
}

fn s(id: &str) -> Value {
    Value::String(id.to_string())
}

const TRICKY: [&str; 9] = [
    "",
    "a",
    "ab",
    "abc",
    "abd",
    "b",
    "\u{e9}",
    "\u{e9}a",
    "\u{65e5}\u{672c}\u{8a9e}",
];

#[test]
fn every_id_resolves_and_neighbours_do_not() {
    let pairs = sorted(&TRICKY);
    let base = base_of(&pairs, 100).unwrap();
    for (id, node) in &pairs {
        let expected = Some(NodeIndex::new(*node as usize));
        assert_eq!(base.lookup("T", &s(id)), expected, "{id:?}");
        assert_eq!(base.lookup_exact("T", &s(id)), expected, "{id:?}");
    }
    for absent in [
        "x", "aa", "abcd", "ab\u{0}", "\u{e9}b", "\u{65e5}", "B", " ",
    ] {
        assert_eq!(base.lookup("T", &s(absent)), None, "{absent:?}");
    }
    assert_eq!(base.entry_len("T"), Some(TRICKY.len()));
}

#[test]
fn only_a_string_matches_a_string_entry() {
    let base = base_of(&sorted(&["1", "2", "7"]), 100).unwrap();
    for other in [
        Value::Int64(1),
        Value::UniqueId(2),
        Value::Float64(7.0),
        Value::Boolean(true),
        Value::Null,
    ] {
        assert_eq!(base.lookup("T", &other), None, "{other:?}");
        assert_eq!(base.lookup_exact("T", &other), None, "{other:?}");
    }
    assert!(base.lookup("T", &s("7")).is_some());
}

#[test]
fn materializing_yields_the_same_map_a_lazy_build_would() {
    let pairs = sorted(&TRICKY);
    let base = base_of(&pairs, 100).unwrap();
    let TypeIdIndex::General(map) = base.materialize("T").unwrap() else {
        panic!("a string entry materializes as a general map");
    };
    assert_eq!(map.len(), pairs.len());
    for (id, node) in &pairs {
        assert_eq!(map.get(&s(id)), Some(&NodeIndex::new(*node as usize)));
    }
}

fn is_refused(keys: Vec<u8>, nodes: Vec<u8>, bound: usize) -> bool {
    let (k, n) = sections(keys, nodes);
    IdIndexBase::from_sections(vec![("T".to_string(), k, n)], bound).is_err()
}

fn raw(offsets: &[u64], bytes: &[u8]) -> Vec<u8> {
    let mut keys: Vec<u8> = offsets.iter().flat_map(|o| o.to_le_bytes()).collect();
    keys.extend_from_slice(bytes);
    keys
}

fn nodes(list: &[u32]) -> Vec<u8> {
    list.iter().flat_map(|n| n.to_le_bytes()).collect()
}

#[test]
fn a_damaged_but_checksummed_section_is_refused() {
    assert!(
        !is_refused(raw(&[0, 1, 2], b"ab"), nodes(&[0, 1]), 5),
        "the control is valid"
    );
    // Not ascending, and a repeat.
    assert!(is_refused(raw(&[0, 1, 2], b"ba"), nodes(&[0, 1]), 5));
    assert!(is_refused(raw(&[0, 1, 2], b"aa"), nodes(&[0, 1]), 5));
    // Offsets that decrease, run past the bytes, do not start at 0 or do not
    // end at the byte length.
    assert!(is_refused(raw(&[0, 2, 1], b"ab"), nodes(&[0, 1]), 5));
    assert!(is_refused(raw(&[0, 1, 3], b"ab"), nodes(&[0, 1]), 5));
    assert!(is_refused(raw(&[1, 1, 2], b"ab"), nodes(&[0, 1]), 5));
    assert!(is_refused(raw(&[0, 1, 1], b"ab"), nodes(&[0, 1]), 5));
    // Fewer offsets than nodes need, a ragged node section.
    assert!(is_refused(raw(&[0, 1], b"a"), nodes(&[0, 1]), 5));
    let mut ragged = nodes(&[0, 1]);
    ragged.pop();
    assert!(is_refused(raw(&[0, 1, 2], b"ab"), ragged, 5));
    // A key that is not UTF-8, a node outside the graph.
    assert!(is_refused(raw(&[0, 1, 2], b"a\xff"), nodes(&[0, 1]), 5));
    assert!(is_refused(raw(&[0, 1, 2], b"ab"), nodes(&[0, 5]), 5));
}

fn store_over(pairs: &[(String, u32)], bound: usize) -> IdIndexStore {
    IdIndexStore::from_base(base_of(pairs, bound).unwrap())
}

#[test]
fn writes_layer_over_a_string_entry_without_copying_it() {
    let mut store = store_over(&sorted(&["a", "b", "c"]), 100);
    let before = full_maps_built();
    let (b, c) = (NodeIndex::new(11), NodeIndex::new(12));
    assert_eq!(store.lookup("T", &s("b")), Some(b));

    store
        .entry_or_default("T".to_string())
        .insert(s("d"), NodeIndex::new(50));
    assert_eq!(store.lookup("T", &s("d")), Some(NodeIndex::new(50)));
    assert_eq!(store.lookup("T", &s("b")), Some(b));
    assert!(store
        .entry_or_default("T".to_string())
        .remove_matching(&s("c"), c));
    assert_eq!(store.lookup("T", &s("c")), None);
    store
        .entry_or_default("T".to_string())
        .insert(s("a"), NodeIndex::new(60));
    assert_eq!(store.lookup("T", &s("a")), Some(NodeIndex::new(60)));
    // A non-string id joins the type next to the strings.
    store
        .entry_or_default("T".to_string())
        .insert(Value::Int64(7), NodeIndex::new(70));
    assert_eq!(
        store.lookup("T", &Value::Int64(7)),
        Some(NodeIndex::new(70))
    );
    assert_eq!(store.lookup("T", &s("b")), Some(b));

    assert_eq!(store.editable_len("T"), Some(4), "a, b, d and 7 remain");
    assert_eq!(
        full_maps_built(),
        before,
        "no write copied the mapped ids onto the heap"
    );

    let evicted = store.evict_entries("T", &[(s("b"), b)]);
    assert!(evicted);
    assert_eq!(store.lookup("T", &s("b")), None);
    assert_eq!(full_maps_built(), before);
}

#[test]
fn a_disk_save_of_a_string_entry_reads_back_the_same_lookups() {
    let pairs = sorted(&TRICKY);
    let mut interner = StringInterner::new();
    interner.try_get_or_intern("T").unwrap();
    let mut store = store_over(&pairs, 100);
    // One layered edit, to take the merged path as well as the untouched one.
    store
        .entry_or_default("T".to_string())
        .insert(s("zz"), NodeIndex::new(90));
    for edited in [false, true] {
        let mut types = TypeIndexStore::default();
        for node in 0..(pairs.len() + usize::from(edited)) {
            types.push_to_type("T", NodeIndex::new(node));
        }
        let store = if edited {
            &store
        } else {
            &store_over(&pairs, 100)
        };
        let dir = tempfile::tempdir().unwrap();
        write_id_indices_bin(dir.path(), store, &types, &interner).unwrap();
        let back = IdIndexBase::load_from(dir.path(), &interner)
            .unwrap()
            .expect("the file was written");
        for (id, node) in &pairs {
            assert_eq!(
                back.lookup("T", &s(id)),
                Some(NodeIndex::new(*node as usize)),
                "{id:?} edited={edited}"
            );
        }
        assert_eq!(
            back.lookup("T", &s("zz")),
            edited.then(|| NodeIndex::new(90))
        );
    }
}
