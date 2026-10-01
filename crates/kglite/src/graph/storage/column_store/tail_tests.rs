//! Rows appended to an mmap-backed store live in its tail. Every reader and
//! writer must answer for the union exactly as a plain heap store holding the
//! same rows does, and a save must write base regions and tail regions into
//! one file whose reload agrees with that heap store again.

use super::*;
use crate::graph::io::unified_columns::write_unified_columns;
use crate::graph::schema::TypeSchema;
use chrono::NaiveDate;
use memmap2::MmapOptions;

fn key(name: &str) -> InternedKey {
    InternedKey::from_str(name)
}

fn hired(i: i64) -> Value {
    Value::Timestamp(
        NaiveDate::from_ymd_opt(2001, 1, 1)
            .unwrap()
            .and_hms_micro_opt(8, 0, 0, (i % 1_000_000) as u32)
            .unwrap()
            + chrono::Duration::days(i),
    )
}

/// Org-chart row `i`: a badge-number id, a name title, a department, a level,
/// a hire timestamp and (for some) a bonus. The same row goes into the heap
/// reference and, for the rows past the base, the tail.
fn employee(i: i64) -> (Value, Value, Vec<(InternedKey, Value)>) {
    let departments = ["Sales", "Engineering", "Finance", "People"];
    let mut properties = vec![
        (
            key("dept"),
            Value::String(departments[(i % 4) as usize].to_string()),
        ),
        (key("level"), Value::Int64(i % 7)),
        (key("hired"), hired(i)),
    ];
    if i % 3 == 0 {
        properties.push((key("bonus"), Value::Float64(i as f64 * 1.5)));
    }
    (
        Value::Int64(3_000_000_000_000 + i),
        Value::String(format!("Employee {i}")),
        properties,
    )
}

fn push(store: &mut ColumnStore, i: i64) -> u32 {
    let (id, title, properties) = employee(i);
    store.push_id(&id);
    store.push_title(&title);
    store.push_row(&properties)
}

fn empty_store() -> ColumnStore {
    ColumnStore::new(
        Arc::new(TypeSchema::new()),
        &HashMap::new(),
        &StringInterner::new(),
    )
}

fn heap_store(rows: std::ops::Range<i64>) -> ColumnStore {
    let mut store = empty_store();
    for i in rows {
        push(&mut store, i);
    }
    store
}

struct Mapped {
    store: ColumnStore,
    _directory: tempfile::TempDir,
}

/// `rows` written to a column file and mapped back: a pure mmap-backed store.
fn mapped_store(rows: std::ops::Range<i64>) -> Mapped {
    let directory = tempfile::tempdir().unwrap();
    let stores = HashMap::from([("Employee".to_string(), Arc::new(heap_store(rows)))]);
    let result =
        write_unified_columns(directory.path(), &stores, &StringInterner::new(), None).unwrap();
    assert!(result.written.contains("Employee"));
    Mapped {
        store: open_type_file(directory.path()),
        _directory: directory,
    }
}

fn open_type_file(dir: &std::path::Path) -> ColumnStore {
    let seg0 = dir.join("seg_000");
    let mut columns =
        crate::graph::io::columns_meta::read(&seg0.join("columns_meta.json")).unwrap();
    let relative = columns.files.remove("Employee").unwrap();
    let file = std::fs::File::open(seg0.join(relative)).unwrap();
    // SAFETY: the test owns this immutable file and keeps its directory alive.
    let map = unsafe { MmapOptions::new().map_copy(&file).unwrap() };
    let meta = columns
        .types
        .into_iter()
        .find(|meta| meta.type_name == "Employee")
        .unwrap();
    ColumnStore::from_mmap_store(Arc::new(
        meta.to_mmap_store(Arc::new(map.make_read_only().unwrap())),
    ))
}

const BASE: i64 = 40;
const APPENDED: i64 = 12;

/// Every per-row reader of `actual` against `expected` for every row.
fn assert_same_rows(actual: &ColumnStore, expected: &ColumnStore) {
    assert_eq!(actual.row_count(), expected.row_count());
    assert_eq!(actual.live_count(), expected.live_count());
    let keys = [
        key("dept"),
        key("level"),
        key("hired"),
        key("bonus"),
        key("remote"),
        key("nickname"),
        key("absent"),
    ];
    for row in 0..expected.row_count() {
        assert_eq!(actual.get_id(row), expected.get_id(row), "id {row}");
        assert_eq!(
            actual.get_title(row),
            expected.get_title(row),
            "title {row}"
        );
        assert_eq!(
            actual.id_borrowed(row).map(|v| v.to_value()),
            expected.id_borrowed(row).map(|v| v.to_value()),
            "id_borrowed {row}"
        );
        assert_eq!(
            actual.title_borrowed(row),
            expected.title_borrowed(row),
            "title_borrowed {row}"
        );
        assert_eq!(
            actual.title_scalar_borrowed(row).map(|v| v.to_value()),
            expected.title_scalar_borrowed(row).map(|v| v.to_value()),
            "title_scalar_borrowed {row}"
        );
        assert_eq!(
            format!("{:?}", actual.title_field(row)),
            format!("{:?}", expected.title_field(row)),
            "title_field {row}"
        );
        assert_eq!(
            format!("{:?}", actual.id_field(row)),
            format!("{:?}", expected.id_field(row)),
            "id_field {row}"
        );
        assert_eq!(
            actual.is_tombstoned(row),
            expected.is_tombstoned(row),
            "tombstone {row}"
        );
        for k in keys {
            assert_eq!(actual.get(row, k), expected.get(row, k), "get {row}");
            assert_eq!(
                actual.get_cow(row, k).map(|v| v.into_owned()),
                expected.get_cow(row, k).map(|v| v.into_owned()),
                "get_cow {row}"
            );
            assert_eq!(
                actual.contains_value(row, k),
                expected.contains_value(row, k),
                "contains_value {row}"
            );
            assert_eq!(
                format!("{:?}", actual.str_field(row, k)),
                format!("{:?}", expected.str_field(row, k)),
                "str_field {row}"
            );
            assert_eq!(
                actual.str_prop_eq(row, k, "Sales"),
                expected.str_prop_eq(row, k, "Sales"),
                "str_prop_eq {row}"
            );
        }
        let sorted = |mut properties: Vec<(InternedKey, Value)>| {
            properties.sort_by_key(|(k, _)| k.as_u64());
            properties
        };
        assert_eq!(
            sorted(actual.row_properties(row)),
            sorted(expected.row_properties(row)),
            "row_properties {row}"
        );
        let mut actual_keys = actual.row_property_keys(row);
        let mut expected_keys = expected.row_property_keys(row);
        actual_keys.sort_by_key(|k| k.as_u64());
        expected_keys.sort_by_key(|k| k.as_u64());
        assert_eq!(actual_keys, expected_keys, "row_property_keys {row}");
        assert_eq!(
            actual.row_property_count(row),
            expected.row_property_count(row)
        );
        let mut visited = Vec::new();
        actual
            .try_for_each_property_borrowed(row, |k, v| {
                visited.push((k, v.to_value()));
                Ok::<(), ()>(())
            })
            .unwrap();
        let mut expected_visited = Vec::new();
        expected
            .try_for_each_property_borrowed(row, |k, v| {
                expected_visited.push((k, v.to_value()));
                Ok::<(), ()>(())
            })
            .unwrap();
        assert_eq!(sorted(visited), sorted(expected_visited), "visitor {row}");
    }
}

fn assert_same_identity_kinds(actual: &ColumnStore, expected: &ColumnStore) {
    assert_eq!(actual.id_type_str(), expected.id_type_str());
    assert_eq!(actual.title_type_str(), expected.title_type_str());
}

/// An interner that knows every property the fixture uses.
fn interner() -> StringInterner {
    let mut interner = StringInterner::new();
    for name in [
        "dept", "level", "hired", "bonus", "remote", "nickname", "absent", "extra",
    ] {
        interner.get_or_intern(name);
    }
    interner
}

fn appended(mapped: &mut ColumnStore) {
    for i in BASE..BASE + APPENDED {
        let row = push(mapped, i);
        assert_eq!(row as i64, i, "a pushed row id continues the base's");
    }
}

#[test]
fn an_append_to_a_mapped_store_leaves_the_base_alone_and_reads_as_the_union() {
    let mut fixture = mapped_store(0..BASE);
    let store = &mut fixture.store;
    assert!(store.pure_mmap_store().is_some());
    appended(store);

    assert!(store.has_mmap_base());
    assert_eq!(store.tail_rows(), APPENDED as u32);
    assert!(
        store.pure_mmap_store().is_none(),
        "a store with tail rows is no longer just its base"
    );
    assert!(
        store.columns_ref().len() == 0 && store.id_column_ref().is_none(),
        "the base part gained no overlay column"
    );
    assert_same_rows(store, &heap_store(0..BASE + APPENDED));
    assert_same_identity_kinds(store, &heap_store(0..BASE + APPENDED));
}

#[test]
fn writes_tombstones_and_truncation_route_to_the_part_that_owns_the_row() {
    let mut fixture = mapped_store(0..BASE);
    let store = &mut fixture.store;
    appended(store);
    let mut expected = heap_store(0..BASE + APPENDED);

    // A SET on a tail row and on a base row, a new key on each, a title set,
    // and a tombstone that is taken back.
    for (row, k, v) in [
        (BASE as u32 + 2, key("level"), Value::Int64(99)),
        (BASE as u32 + 3, key("remote"), Value::Boolean(true)),
        (5, key("level"), Value::Int64(-1)),
        (6, key("remote"), Value::Boolean(false)),
    ] {
        assert!(store.set(row, k, &v, None));
        assert!(expected.set(row, k, &v, None));
    }
    let title = Value::String("Renamed".to_string());
    for row in [BASE as u32 + 4, 7] {
        assert!(store.set_title(row, &title), "set_title {row}");
        assert!(expected.set_title(row, &title));
    }
    store.tombstone(BASE as u32 + 5);
    expected.tombstone(BASE as u32 + 5);
    assert!(store.is_tombstoned(BASE as u32 + 5));
    assert_same_rows(store, &expected);
    store.untombstone(BASE as u32 + 5);
    expected.untombstone(BASE as u32 + 5);
    assert_same_rows(store, &expected);

    // A slot-addressed write and read on a tail row carry the key across.
    let level = store
        .slot(key("level"))
        .expect("a SET made the base overlay column");
    assert!(store.set_at_slot(BASE as u32 + 6, level, &Value::Int64(17)));
    assert!(expected.set_at_slot(
        BASE as u32 + 6,
        expected.slot(key("level")).unwrap(),
        &Value::Int64(17)
    ));
    assert_eq!(
        store.get_by_slot(BASE as u32 + 6, level),
        Some(Value::Int64(17))
    );

    // Out of range is still out of range.
    assert!(!store.set(
        BASE as u32 + APPENDED as u32,
        key("level"),
        &Value::Int64(1),
        None
    ));
    assert_eq!(store.get(BASE as u32 + APPENDED as u32, key("level")), None);

    // Truncating to the base drops the tail; the store is a pure base again
    // (the SET overlay made it impure, so compare what the tail contributes).
    store.truncate_rows(BASE as u32 + 4);
    expected.truncate_rows(BASE as u32 + 4);
    assert_same_rows(store, &expected);
    store.truncate_rows(BASE as u32);
    expected.truncate_rows(BASE as u32);
    assert_eq!(store.tail_rows(), 0, "an empty tail is dropped");
    assert_same_rows(store, &expected);
}

#[test]
fn a_rolled_back_first_append_leaves_a_pure_mapped_store() {
    let mut fixture = mapped_store(0..BASE);
    let store = &mut fixture.store;
    appended(store);
    store.truncate_rows(BASE as u32);
    assert!(
        store.pure_mmap_store().is_some(),
        "re-emitted verbatim by the next save"
    );
    assert_eq!(store.row_count(), BASE as u32);
}

/// Save the store, map the file back and compare it to the heap reference.
fn saved_and_reloaded(store: &ColumnStore) -> Mapped {
    let directory = tempfile::tempdir().unwrap();
    let stores = HashMap::from([("Employee".to_string(), Arc::new(store.clone()))]);
    let result =
        write_unified_columns(directory.path(), &stores, &StringInterner::new(), None).unwrap();
    assert!(
        result.written.contains("Employee"),
        "the store was written as column regions, not a sidecar: {:?}",
        result.unhandled
    );
    Mapped {
        store: open_type_file(directory.path()),
        _directory: directory,
    }
}

#[test]
fn a_save_writes_the_base_and_tail_regions_into_one_file() {
    let mut fixture = mapped_store(0..BASE);
    appended(&mut fixture.store);
    assert!(
        fixture.store.region_parts().is_some(),
        "same-kind columns: the regions concatenate"
    );
    let reloaded = saved_and_reloaded(&fixture.store);
    assert!(reloaded.store.pure_mmap_store().is_some());
    assert_same_rows(&reloaded.store, &heap_store(0..BASE + APPENDED));

    // A second cycle: append to the reloaded store and save again.
    let mut again = reloaded;
    for i in BASE + APPENDED..BASE + APPENDED + 5 {
        push(&mut again.store, i);
    }
    let twice = saved_and_reloaded(&again.store);
    assert_same_rows(&twice.store, &heap_store(0..BASE + APPENDED + 5));
}

#[test]
fn a_column_one_part_lacks_is_written_as_nulls_for_that_part() {
    let mut fixture = mapped_store(0..BASE);
    let store = &mut fixture.store;
    // The tail's first rows carry a key the base never had; later rows omit
    // `bonus`, which the base has for every third row.
    let (id, title, mut properties) = employee(BASE);
    properties.retain(|(k, _)| *k != key("bonus"));
    properties.push((key("remote"), Value::Boolean(true)));
    store.push_id(&id);
    store.push_title(&title);
    store.push_row(&properties);
    let mut expected = heap_store(0..BASE);
    expected.push_id(&id);
    expected.push_title(&title);
    expected.push_row(&properties);

    assert!(store.region_parts().is_some());
    let reloaded = saved_and_reloaded(store);
    assert_same_rows(&reloaded.store, &expected);
    assert_eq!(
        reloaded.store.get(BASE as u32, key("remote")),
        Some(Value::Boolean(true))
    );
    assert_eq!(reloaded.store.get(3, key("remote")), None);
    assert_eq!(reloaded.store.get(BASE as u32, key("bonus")), None);
    assert_eq!(
        reloaded.store.get(3, key("bonus")),
        Some(Value::Float64(4.5))
    );
}

#[test]
fn a_tail_column_of_another_kind_than_the_base_is_flattened_not_concatenated() {
    let mut fixture = mapped_store(0..BASE);
    let store = &mut fixture.store;
    let (id, title, _) = employee(BASE);
    // `level` is an integer in the base; a float cannot share its region.
    store.push_id(&id);
    store.push_title(&title);
    store.push_row(&[(key("level"), Value::Float64(2.5))]);
    assert!(
        store.region_parts().is_none(),
        "mixed kinds take the flatten path, which reads through the routed accessors"
    );
    let flat = store.flattened_owned(&HashMap::new(), &interner());
    assert_eq!(flat.row_count(), BASE as u32 + 1);
    assert_eq!(
        flat.get(BASE as u32, key("level")),
        Some(Value::Float64(2.5))
    );
    assert_eq!(flat.get(4, key("level")), Some(Value::Int64(4)));
    assert_eq!(flat.get_title(BASE as u32), Some(title));
}

#[test]
fn a_key_the_base_lacks_typed_differently_by_overlay_and_tail_is_flattened() {
    for (overlay_value, tail_value) in [
        (Value::Int64(5), Value::String("x".to_string())),
        (Value::String("x".to_string()), Value::Int64(5)),
    ] {
        let mut fixture = mapped_store(0..BASE);
        let store = &mut fixture.store;
        assert!(store.set(0, key("extra"), &overlay_value, None));
        let (id, title, _) = employee(BASE);
        store.push_id(&id);
        store.push_title(&title);
        store.push_row(&[(key("extra"), tail_value.clone())]);
        assert_eq!(store.get(0, key("extra")), Some(overlay_value.clone()));
        assert_eq!(
            store.get(BASE as u32, key("extra")),
            Some(tail_value.clone())
        );
        assert!(
            store.region_parts().is_none(),
            "one file column cannot hold {overlay_value:?} over the base rows and \
             {tail_value:?} in the tail: the save flattens, which reads the union"
        );
        let flat = store.flattened_owned(&HashMap::new(), &interner());
        assert_eq!(flat.get(0, key("extra")), Some(overlay_value));
        assert_eq!(flat.get(BASE as u32, key("extra")), Some(tail_value));
    }
}

#[test]
fn a_tail_column_with_no_value_takes_the_base_kind_whatever_it_was_declared() {
    let mut fixture = mapped_store(0..BASE);
    let store = &mut fixture.store;
    // The registered type says `level` is a date; no appended row has a value.
    let schema = Arc::new(TypeSchema::from_keys([key("level"), key("dept")]));
    let meta = HashMap::from([
        ("level".to_string(), "Date".to_string()),
        ("dept".to_string(), "String".to_string()),
    ]);
    store.prepare_append(schema, &meta, &interner());
    let (id, title, _) = employee(BASE);
    store.push_id(&id);
    store.push_title(&title);
    store.push_row(&[(key("dept"), Value::String("Sales".to_string()))]);
    assert!(store.region_parts().is_some());
    let reloaded = saved_and_reloaded(store);
    assert_eq!(reloaded.store.get(BASE as u32, key("level")), None);
    assert_eq!(reloaded.store.get(9, key("level")), Some(Value::Int64(2)));
    assert_eq!(
        reloaded.store.get(BASE as u32, key("dept")),
        Some(Value::String("Sales".to_string()))
    );
}

/// `SET` cells of every shape a save can lay over a base column: an integer, a
/// timestamp, strings longer and shorter than the ones they replace, a float on
/// a row that had none, and two columns the base never had.
fn overlay_changes(store: &mut ColumnStore) {
    let mut set = |row: u32, name: &str, value: Value| {
        assert!(store.set(row, key(name), &value, None), "{name} at {row}");
    };
    set(3, "level", Value::Int64(40));
    set(5, "hired", hired(5000));
    set(7, "dept", Value::String("Engineering Platform".to_string()));
    set(8, "dept", Value::String("HR".to_string()));
    set(10, "bonus", Value::Float64(99.5));
    set(10, "remote", Value::Boolean(true));
    set(11, "nickname", Value::String("Ace".to_string()));
}

#[test]
fn a_set_over_a_mapped_base_is_written_from_regions_and_reloads_as_the_heap_store_does() {
    let mut fixture = mapped_store(0..BASE);
    overlay_changes(&mut fixture.store);
    let mut expected = heap_store(0..BASE);
    overlay_changes(&mut expected);

    assert!(fixture.store.pure_mmap_store().is_none());
    assert!(
        fixture.store.region_parts().is_some(),
        "same-kind overlay columns are laid over the base regions"
    );
    let reloaded = saved_and_reloaded(&fixture.store);
    assert!(reloaded.store.pure_mmap_store().is_some());
    assert_same_rows(&reloaded.store, &expected);
    assert_eq!(
        reloaded.store.get(7, key("dept")),
        Some(Value::String("Engineering Platform".to_string()))
    );
    assert_eq!(
        reloaded.store.get(8, key("dept")),
        Some(Value::String("HR".to_string()))
    );
    assert_eq!(
        reloaded.store.get(9, key("dept")),
        Some(Value::String("Engineering".to_string())),
        "a string the overlay left alone is the base's"
    );

    // A second cycle: change the reloaded store again and save again.
    let mut again = reloaded;
    assert!(again.store.set(3, key("level"), &Value::Int64(41), None));
    assert!(again
        .store
        .set(3, key("nickname"), &Value::String("Zed".to_string()), None));
    let mut expected_again = expected;
    assert!(expected_again.set(3, key("level"), &Value::Int64(41), None));
    assert!(expected_again.set(3, key("nickname"), &Value::String("Zed".to_string()), None));
    let twice = saved_and_reloaded(&again.store);
    assert_same_rows(&twice.store, &expected_again);
}

#[test]
fn a_set_base_with_a_tail_is_written_from_regions_in_one_file() {
    let mut fixture = mapped_store(0..BASE);
    let store = &mut fixture.store;
    overlay_changes(store);
    appended(store);
    // A SET on an appended row lands in the tail, not the overlay.
    assert!(store.set(BASE as u32 + 2, key("level"), &Value::Int64(17), None));
    let mut expected = heap_store(0..BASE + APPENDED);
    overlay_changes(&mut expected);
    assert!(expected.set(BASE as u32 + 2, key("level"), &Value::Int64(17), None));

    assert!(store
        .region_parts()
        .is_some_and(|parts| parts.tail.is_some()));
    let reloaded = saved_and_reloaded(store);
    assert_same_rows(&reloaded.store, &expected);
}

#[test]
fn a_store_the_regions_cannot_hold_is_flattened_and_still_right() {
    type Change = fn(&mut ColumnStore);
    let cases: [(&str, Change); 4] = [
        ("a null written over a base cell", |store| {
            assert!(store.set(3, key("level"), &Value::Null, None));
        }),
        ("a replaced title beside an overlaid column", |store| {
            assert!(store.set(4, key("level"), &Value::Int64(9), None));
            assert!(store.set_title(3, &Value::String("Renamed".to_string())));
        }),
        ("a value of another kind in an integer column", |store| {
            assert!(store.set(3, key("level"), &Value::String("L3".to_string()), None));
        }),
        (
            "an overlay and a column of another kind in the tail",
            |store| {
                assert!(store.set(4, key("level"), &Value::Int64(9), None));
                let (id, title, _) = employee(BASE);
                store.push_id(&id);
                store.push_title(&title);
                store.push_row(&[(key("level"), Value::Float64(2.5))]);
            },
        ),
    ];
    for (what, change) in cases {
        let mut fixture = mapped_store(0..BASE);
        change(&mut fixture.store);
        let mut expected = heap_store(0..BASE);
        if what.contains("another kind in the tail") {
            assert!(expected.set(4, key("level"), &Value::Int64(9), None));
            let (id, title, _) = employee(BASE);
            expected.push_id(&id);
            expected.push_title(&title);
            expected.push_row(&[(key("level"), Value::Float64(2.5))]);
        } else {
            change(&mut expected);
        }
        assert!(
            fixture.store.region_parts().is_none(),
            "{what} cannot be laid over the base regions"
        );
        let flat = fixture.store.flattened_owned(&HashMap::new(), &interner());
        assert_same_rows(&flat, &expected);
    }
}

/// The validity filter and the endpoint-index build read a typed timestamp
/// column directly. Over an mmap base the overlay column holds only the cells a
/// `SET` wrote (null elsewhere), so answering from it alone would read every
/// other row as unbounded. No query on a disk graph reaches this read today —
/// disk residual guards, scans and the endpoint index all bypass it — which is
/// why the guard is pinned here, at the store.
#[test]
fn an_mmap_backed_store_never_answers_a_timestamp_cell_from_its_overlay_alone() {
    let mut fixture = mapped_store(0..BASE);
    let store = &mut fixture.store;
    assert!(store.timestamp_cells(key("hired")).is_none(), "pure base");
    assert!(store.set(3, key("hired"), &hired(9000), None));
    assert!(
        store.timestamp_cells(key("hired")).is_none(),
        "an overlaid base is read through the routed accessors, which see the base cells"
    );
    assert_eq!(store.timestamp_micros(7, key("hired")), None);
    appended(store);
    assert!(
        store.timestamp_cells(key("hired")).is_none(),
        "base and tail"
    );
    // The routed read answers for every row.
    assert_eq!(store.get(3, key("hired")), Some(hired(9000)));
    assert_eq!(store.get(7, key("hired")), Some(hired(7)));
    assert_eq!(
        store.get(BASE as u32 + 1, key("hired")),
        Some(hired(BASE + 1))
    );
}

#[test]
fn a_fork_shares_the_tail_and_the_first_append_on_either_side_copies_only_it() {
    let mut fixture = mapped_store(0..BASE);
    appended(&mut fixture.store);
    let mut fork = fixture.store.clone();
    let before = column_clones_since_reset(|| {
        push(&mut fork, BASE + APPENDED);
    });
    assert_eq!(fixture.store.row_count(), (BASE + APPENDED) as u32);
    assert_eq!(fork.row_count(), (BASE + APPENDED) as u32 + 1);
    assert_eq!(fixture.store.get_title((BASE + APPENDED) as u32), None);
    assert_eq!(
        fork.get_title((BASE + APPENDED) as u32),
        Some(Value::String(format!("Employee {}", BASE + APPENDED)))
    );
    assert!(before > 0, "the fork privatised the tail, and only it");
}

fn column_clones_since_reset(work: impl FnOnce()) -> usize {
    reset_column_store_clones();
    work();
    column_store_clones()
}

#[test]
fn a_type_change_in_the_tail_is_logged_and_restored() {
    let mut fixture = mapped_store(0..BASE);
    let store = &mut fixture.store;
    appended(store);
    store.begin_displaced_log();
    // A string where the tail's `level` column holds integers demotes it.
    assert!(store.set(
        BASE as u32 + 1,
        key("level"),
        &Value::String("L2".to_string()),
        None
    ));
    assert!(store.has_displaced());
    let log = store.take_displaced();
    assert!(!log.is_empty());
    store.end_displaced_log();
    let expected_before = heap_store(0..BASE + APPENDED);
    // Put the cell back, then the column's type.
    assert!(store.set(BASE as u32 + 1, key("level"), &Value::Int64(1), None));
    for displaced in log.into_iter().rev() {
        store.restore_displaced(displaced);
    }
    assert_same_rows(store, &expected_before);
    assert!(
        store.region_parts().is_some(),
        "the tail's column is typed again"
    );
}
