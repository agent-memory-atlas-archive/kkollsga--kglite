//! Rows appended to an mmap-backed store live in its tail. Every reader and
//! writer must answer for the union exactly as a plain heap store holding the
//! same rows does, and a save must write base regions and tail regions into
//! one file whose reload agrees with that heap store again.

use super::*;
use crate::graph::io::unified_columns::write_unified_columns_published;
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
    let result = write_unified_columns_published(directory.path(), &stores, None).unwrap();
    assert!(result.files.contains_key("Employee"));
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
        meta.to_mmap_store(Arc::new(map.make_read_only().unwrap()))
            .unwrap(),
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

    // A slot-addressed write on a tail row carries the key across.
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
        store.get(BASE as u32 + 6, key("level")),
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
    let result = write_unified_columns_published(directory.path(), &stores, None).unwrap();
    assert!(
        result.files.contains_key("Employee"),
        "the store was written as column regions, not a sidecar"
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

/// A tombstone on a base row of the mapping, on an appended row, and a SET
/// beside them, on both stores.
fn tombstone_rows(store: &mut ColumnStore, expected: &mut ColumnStore) {
    for row in [7, BASE as u32 + 3, BASE as u32 + 9] {
        store.tombstone(row);
        expected.tombstone(row);
        assert!(store.is_tombstoned(row), "tombstone {row} is held");
    }
    let (level, forty) = (key("level"), Value::Int64(40));
    assert!(store.set(8, level, &forty, None));
    assert!(expected.set(8, level, &forty, None));
}

#[test]
fn a_tombstone_on_a_base_or_appended_row_is_held_and_answered_like_a_heap_store() {
    let mut fixture = mapped_store(0..BASE);
    let store = &mut fixture.store;
    appended(store);
    let mut expected = heap_store(0..BASE + APPENDED);
    tombstone_rows(store, &mut expected);
    assert_same_rows(store, &expected);
    assert_eq!(store.live_count(), BASE as u32 + APPENDED as u32 - 3);

    // Taken back, base row and appended row alike.
    for row in [7, BASE as u32 + 3] {
        store.untombstone(row);
        expected.untombstone(row);
    }
    assert_same_rows(store, &expected);

    // A tombstone does not survive a truncation of the rows it was set on.
    store.tombstone(BASE as u32 + 9);
    expected.tombstone(BASE as u32 + 9);
    store.truncate_rows(BASE as u32 + 5);
    expected.truncate_rows(BASE as u32 + 5);
    assert_same_rows(store, &expected);
}

#[test]
fn a_tombstone_on_a_pure_mapped_base_row_is_held() {
    let mut fixture = mapped_store(0..BASE);
    let mut expected = heap_store(0..BASE);
    fixture.store.tombstone(11);
    expected.tombstone(11);
    assert!(fixture.store.is_tombstoned(11));
    assert!(!fixture.store.is_tombstoned(12));
    assert_eq!(fixture.store.live_count(), BASE as u32 - 1);
    assert_same_rows(&fixture.store, &expected);
    // Out of range stays out of range and is not a tombstone.
    fixture.store.tombstone(BASE as u32 + 100);
    assert_eq!(fixture.store.live_count(), BASE as u32 - 1);
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

/// A copy lays a store's columns out from `property_layout`, and a save writes
/// them through `column_for_plan`: both give a column that holds no value the
/// kind of the base's column, so a copy never types as `mixed` what the save
/// keeps as the base's kind.
#[test]
fn the_layout_a_copy_uses_agrees_with_the_save_on_a_column_with_no_value() {
    let mut fixture = mapped_store(0..BASE);
    let store = &mut fixture.store;
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
    assert!(
        store.region_parts().is_some(),
        "the save keeps the base kind"
    );

    let kind_of = |store: &ColumnStore, name: &str| {
        store
            .property_layout()
            .into_iter()
            .find_map(|(k, kind)| (k == key(name)).then_some(kind))
    };
    assert_eq!(kind_of(store, "level"), Some("int64"));
    let reloaded = saved_and_reloaded(store);
    assert_eq!(kind_of(&reloaded.store, "level"), Some("int64"));
    for name in ["level", "dept", "hired", "bonus"] {
        assert_eq!(
            kind_of(store, name),
            kind_of(&reloaded.store, name),
            "{name}: the copy's layout and the saved file's disagree"
        );
    }
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

/// What the replay of `journal` does to `store`, for the entries a columnar
/// write and a columnar append produce.
fn replay(journal: crate::graph::storage::undo::UndoJournal, store: &mut ColumnStore) {
    use crate::graph::storage::undo::UndoEntry;
    for entry in journal.into_replay_order() {
        match entry {
            UndoEntry::ColumnarCell {
                row_id, key, prior, ..
            } => {
                store.set(row_id, key, &prior.unwrap_or(Value::Null), None);
            }
            UndoEntry::ColumnarSchemaGrown {
                prior_schema,
                prior_column_count,
                part,
                ..
            } => store.restore_schema_of(part, prior_schema, prior_column_count),
            UndoEntry::ColumnarRowsAppended {
                prior_row_count,
                prior_schema,
                prior_column_count,
                prior_tail,
                ..
            } => {
                store.truncate_rows(prior_row_count);
                store.restore_schema(prior_schema, prior_column_count);
                if let Some((part, schema, count)) = prior_tail {
                    store.restore_schema_of(part, schema, count);
                }
            }
            other => panic!("unexpected entry: {other:?}"),
        }
    }
}

#[test]
fn undoing_a_write_that_grew_the_tail_schema_drops_the_tail_column() {
    use crate::graph::storage::undo::{ColumnarPreImages, ColumnarWrite, UndoJournal};
    let mut fixture = mapped_store(0..BASE);
    let store = &mut fixture.store;
    appended(store);
    let brand_new = key("brand_new");
    let node_type = key("Employee");
    let tail_row = BASE as u32 + 3;

    // A key the overlay already holds is still new to the tail's own schema.
    let mut journal = UndoJournal::new();
    for new_key in [brand_new, key("dept_overlay")] {
        if new_key != brand_new {
            assert!(store.set(0, new_key, &Value::Int64(1), None));
        }
        ColumnarPreImages::capture(store, tail_row, ColumnarWrite::Cell(new_key)).record(
            &mut journal,
            node_type,
            tail_row,
        );
        assert!(store.set(tail_row, new_key, &Value::Int64(7), None));
        assert!(store.has_property_column(new_key));
    }
    replay(journal, store);

    for new_key in [brand_new, key("dept_overlay")] {
        assert_eq!(store.get(tail_row, new_key), None);
        assert!(
            !store.tail.as_deref().unwrap().has_property_column(new_key),
            "the tail's schema is back to what it was before the statement"
        );
    }
    // The tail's own columns are untouched.
    assert_eq!(
        store.get(tail_row, key("level")),
        employee(tail_row.into())
            .2
            .into_iter()
            .find_map(|(k, v)| (k == key("level")).then_some(v))
    );
}

#[test]
fn undoing_an_append_that_grew_the_tail_schema_drops_the_tail_column() {
    use crate::graph::storage::undo::{ColumnarAppendPreImage, UndoJournal};
    let mut fixture = mapped_store(0..BASE);
    let store = &mut fixture.store;
    appended(store);
    let mut journal = UndoJournal::new();
    ColumnarAppendPreImage::capture(store).record(&mut journal, key("Employee"), false);
    let (id, title, _) = employee(BASE + APPENDED);
    store.push_id(&id);
    store.push_title(&title);
    store.push_row(&[(key("brand_new"), Value::Int64(1))]);
    assert!(store.has_property_column(key("brand_new")));

    replay(journal, store);

    assert_eq!(store.row_count(), (BASE + APPENDED) as u32);
    assert!(
        !store.has_property_column(key("brand_new")),
        "the rows are gone and so is the column they introduced"
    );
}

// ── Segmented tails ────────────────────────────────────────────────────────
//
// A tail past `TAIL_SEGMENT_ROWS` rows is a run of sealed, shared segments and
// one open segment. Every case here pushes past two seals and compares each
// reader with the plain heap store holding the same rows, which has no tail.

const SEGMENT: i64 = tail::TAIL_SEGMENT_ROWS as i64;
const MANY: i64 = 3 * SEGMENT + 700;

fn appended_many(store: &mut ColumnStore, rows: std::ops::Range<i64>) {
    for i in rows {
        let row = push(store, i);
        assert_eq!(row as i64, i, "a pushed row id continues the previous one");
    }
}

fn segmented_fixture() -> Mapped {
    let mut fixture = mapped_store(0..BASE);
    appended_many(&mut fixture.store, BASE..BASE + MANY);
    assert_eq!(fixture.store.sealed_tails.len(), 3, "three full segments");
    fixture
}

fn assert_tail_shape(store: &ColumnStore) {
    let open = store.tail.as_ref().map_or(0, |open| open.row_count());
    assert!(
        open <= tail::TAIL_SEGMENT_ROWS,
        "the open segment fits one segment"
    );
    for sealed in &store.sealed_tails {
        assert_eq!(
            sealed.row_count(),
            tail::TAIL_SEGMENT_ROWS,
            "sealed segments are full"
        );
    }
    assert_eq!(
        store.tail_rows(),
        store.sealed_tails.len() as u32 * tail::TAIL_SEGMENT_ROWS + open
    );
}

#[test]
fn a_segmented_tail_reads_as_the_union_of_its_segments() {
    let fixture = segmented_fixture();
    assert_tail_shape(&fixture.store);
    assert_same_rows(&fixture.store, &heap_store(0..BASE + MANY));
    assert_same_identity_kinds(&fixture.store, &heap_store(0..BASE + MANY));
    assert_eq!(fixture.store.tail_rows() as i64, MANY);
}

#[test]
fn a_fork_shares_every_sealed_segment_and_an_append_copies_only_the_open_one() {
    let fixture = segmented_fixture();
    let mut fork = fixture.store.clone();
    let clones = column_clones_since_reset(|| {
        push(&mut fork, BASE + MANY);
    });
    assert_eq!(clones, 1, "only the open segment was privatised");
    for (mine, theirs) in fork.sealed_tails.iter().zip(&fixture.store.sealed_tails) {
        assert!(Arc::ptr_eq(mine, theirs), "sealed segments stay shared");
    }
    assert_eq!(fork.row_count() as i64, BASE + MANY + 1);
    assert_eq!(fixture.store.row_count() as i64, BASE + MANY);
    assert_same_rows(&fixture.store, &heap_store(0..BASE + MANY));
    assert_same_rows(&fork, &heap_store(0..BASE + MANY + 1));

    // An append that fills the open segment seals it without touching the rest.
    let room = tail::TAIL_SEGMENT_ROWS - fork.tail.as_ref().unwrap().row_count();
    let mut next = BASE + MANY + 1;
    for _ in 0..=room {
        push(&mut fork, next);
        next += 1;
    }
    assert_eq!(fork.sealed_tails.len(), 4);
    assert_tail_shape(&fork);
    assert_same_rows(&fork, &heap_store(0..next));
}

#[test]
fn writes_tombstones_and_new_keys_route_to_sealed_open_and_base_rows() {
    let mut fixture = segmented_fixture();
    let store = &mut fixture.store;
    let mut expected = heap_store(0..BASE + MANY);
    let sealed_row = BASE as u32 + 5;
    let middle_row = BASE as u32 + tail::TAIL_SEGMENT_ROWS + 9;
    let open_row = (BASE + MANY - 3) as u32;
    for (row, k, v) in [
        (sealed_row, key("level"), Value::Int64(98)),
        (sealed_row, key("remote"), Value::Boolean(true)),
        (middle_row, key("nickname"), Value::String("Mid".into())),
        (open_row, key("level"), Value::Int64(97)),
        (open_row, key("remote"), Value::Boolean(false)),
        (3, key("level"), Value::Int64(-7)),
    ] {
        assert!(store.set(row, k, &v, None), "set {row}");
        assert!(expected.set(row, k, &v, None));
    }
    let title = Value::String("Renamed".to_string());
    for row in [sealed_row + 1, middle_row, open_row, 4] {
        assert!(store.set_title(row, &title));
        assert!(expected.set_title(row, &title));
    }
    for row in [sealed_row + 2, middle_row + 1, open_row - 1] {
        store.tombstone(row);
        expected.tombstone(row);
    }
    assert_same_rows(store, &expected);
    store.untombstone(middle_row + 1);
    expected.untombstone(middle_row + 1);
    let level = store
        .slot(key("level"))
        .expect("a SET made the overlay column");
    assert!(store.set_at_slot(middle_row + 2, level, &Value::Int64(5)));
    assert!(expected.set_at_slot(
        middle_row + 2,
        expected.slot(key("level")).unwrap(),
        &Value::Int64(5)
    ));
    assert_same_rows(store, &expected);
    assert!(!store.set((BASE + MANY) as u32, key("level"), &Value::Int64(1), None));
}

#[test]
fn truncating_across_segment_boundaries_unseals_and_appends_continue() {
    let fixture = segmented_fixture();
    let keeps = [
        BASE + MANY - 1,
        BASE + 3 * SEGMENT,
        BASE + 3 * SEGMENT - 1,
        BASE + 2 * SEGMENT + 1,
        BASE + 2 * SEGMENT,
        BASE + SEGMENT,
        BASE + 1,
        BASE,
    ];
    for keep in keeps {
        let mut store = fixture.store.clone();
        store.truncate_rows(keep as u32);
        assert_tail_shape(&store);
        assert_eq!(store.row_count() as i64, keep, "keep {keep}");
        let expected = heap_store(0..keep);
        assert_same_rows(&store, &expected);
        if keep == BASE {
            assert!(store.tail.is_none() && store.pure_mmap_store().is_some());
        }
        // The next appends land on the rows the truncation vacated.
        appended_many(&mut store, keep..keep + 5);
        assert_tail_shape(&store);
        assert_same_rows(&store, &heap_store(0..keep + 5));
    }
    assert_eq!(
        fixture.store.row_count() as i64,
        BASE + MANY,
        "the source of the clones is untouched"
    );
}

#[test]
fn a_segmented_tail_saves_into_one_file_and_reloads_as_the_heap_store_does() {
    let mut fixture = segmented_fixture();
    fixture
        .store
        .set(BASE as u32 + 3, key("level"), &Value::Int64(1234), None);
    let mut expected = heap_store(0..BASE + MANY);
    expected.set(BASE as u32 + 3, key("level"), &Value::Int64(1234), None);
    let reloaded = saved_and_reloaded(&fixture.store);
    assert_same_rows(&reloaded.store, &expected);
}

#[test]
fn undoing_a_write_that_grew_a_sealed_segments_schema_drops_the_column() {
    use crate::graph::storage::undo::{ColumnarPreImages, ColumnarWrite, UndoJournal};
    let mut fixture = segmented_fixture();
    let store = &mut fixture.store;
    let row = BASE as u32 + 11;
    let mut journal = UndoJournal::new();
    let brand_new = key("brand_new");
    ColumnarPreImages::capture(store, row, ColumnarWrite::Cell(brand_new)).record(
        &mut journal,
        key("Employee"),
        row,
    );
    assert!(store.set(row, brand_new, &Value::Int64(7), None));
    assert_eq!(store.get(row, brand_new), Some(Value::Int64(7)));
    replay(journal, store);
    assert_eq!(store.get(row, brand_new), None);
    assert!(!store.has_property_column(brand_new));
    assert_same_rows(store, &heap_store(0..BASE + MANY));
}

#[test]
fn undoing_an_append_across_segment_boundaries_restores_the_tail() {
    use crate::graph::storage::undo::{ColumnarAppendPreImage, UndoJournal};
    let mut fixture = segmented_fixture();
    let store = &mut fixture.store;
    let mut journal = UndoJournal::new();
    ColumnarAppendPreImage::capture(store).record(&mut journal, key("Employee"), false);
    appended_many(store, BASE + MANY..BASE + MANY + 2 * SEGMENT);
    store.push_id(&Value::Int64(1));
    store.push_title(&Value::String("x".into()));
    store.push_row(&[(key("brand_new"), Value::Int64(1))]);
    assert!(store.has_property_column(key("brand_new")));
    replay(journal, store);
    assert_tail_shape(store);
    assert_eq!(store.row_count() as i64, BASE + MANY);
    assert!(!store.has_property_column(key("brand_new")));
    assert_same_rows(store, &heap_store(0..BASE + MANY));
}

#[test]
fn a_type_change_in_a_sealed_segment_is_logged_and_restored() {
    let mut fixture = segmented_fixture();
    let store = &mut fixture.store;
    store.begin_displaced_log();
    let row = BASE as u32 + 1;
    assert!(store.set(row, key("level"), &Value::String("L2".to_string()), None));
    assert!(store.has_displaced());
    let log = store.take_displaced();
    assert!(!log.is_empty());
    store.end_displaced_log();
    assert!(store.set(row, key("level"), &Value::Int64(1), None));
    for displaced in log.into_iter().rev() {
        store.restore_displaced(displaced);
    }
    assert_same_rows(store, &heap_store(0..BASE + MANY));
}

/// A random run of appends, sets, tombstones, truncations and forks over a
/// segmented tail reads as the plain heap store the same run builds, and no
/// fork's write reaches the store it came from.
#[test]
fn random_writes_over_a_segmented_tail_match_a_heap_store() {
    let mut state = 0xD1B5_4A32_D192_ED03u64;
    let mut next = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n
    };
    for round in 0..6 {
        let mut fixture = mapped_store(0..BASE);
        let store = &mut fixture.store;
        let mut expected = heap_store(0..BASE);
        let mut rows = BASE;
        for _ in 0..140 {
            match next(8) {
                0..=2 => {
                    let n = 1 + next(900) as i64;
                    appended_many(store, rows..rows + n);
                    appended_many(&mut expected, rows..rows + n);
                    rows += n;
                }
                3 => {
                    let row = next(rows as u64) as u32;
                    let value = Value::Int64(next(1000) as i64);
                    assert_eq!(
                        store.set(row, key("level"), &value, None),
                        expected.set(row, key("level"), &value, None)
                    );
                }
                4 => {
                    let row = next(rows as u64) as u32;
                    let value = Value::Boolean(next(2) == 0);
                    store.set(row, key("remote"), &value, None);
                    expected.set(row, key("remote"), &value, None);
                }
                5 => {
                    // A base row of an mmap store takes no tombstone of its own
                    // (its node is what goes), so only appended rows are marked.
                    let row = (BASE as u64 + next((rows - BASE + 1) as u64)) as u32;
                    if (row as i64) < rows {
                        store.tombstone(row);
                        expected.tombstone(row);
                    }
                }
                6 => {
                    let keep = BASE + next((rows - BASE + 1) as u64) as i64;
                    store.truncate_rows(keep as u32);
                    expected.truncate_rows(keep as u32);
                    rows = keep;
                }
                _ => {
                    // A fork writes; the original must not move.
                    let mut fork = store.clone();
                    let row = next(rows as u64) as u32;
                    fork.set(row, key("level"), &Value::Int64(-1), None);
                    appended_many(&mut fork, rows..rows + 3);
                    fork.truncate_rows(rows as u32);
                    assert_eq!(
                        store.get(row, key("level")),
                        expected.get(row, key("level"))
                    );
                }
            }
            assert_tail_shape(store);
            assert_eq!(store.row_count(), expected.row_count(), "round {round}");
        }
        assert_same_rows(store, &expected);
    }
}

/// The sealed segments' cached byte totals agree with a fresh sum after every
/// kind of change that reaches a sealed segment, so the statement-level memory
/// checks see what a walk of the segments would.
fn assert_totals_current(store: &ColumnStore, what: &str) {
    let fresh = store.clone();
    fresh.sealed_stats.invalidate();
    assert_eq!(
        store.heap_bytes(),
        fresh.heap_bytes(),
        "heap bytes after {what}"
    );
    assert_eq!(
        store.spillable_heap_bytes(),
        fresh.spillable_heap_bytes(),
        "spillable bytes after {what}"
    );
    assert_eq!(
        store.sealed_totals(),
        fresh.sealed_totals(),
        "totals after {what}"
    );
}

#[test]
fn the_cached_totals_of_the_sealed_segments_follow_every_change_to_them() {
    let mut fixture = segmented_fixture();
    let store = &mut fixture.store;
    assert_totals_current(store, "the appends");
    assert!(store.sealed_totals().0 > 0);
    // A write that widens a column of a sealed segment changes its bytes.
    let row = BASE as u32 + 3;
    assert!(store.set(
        row,
        key("level"),
        &Value::String("a-longer-text-value".into()),
        None
    ));
    assert_totals_current(store, "a type change in a sealed segment");
    store.truncate_rows((BASE + SEGMENT) as u32 + 5);
    assert_totals_current(store, "a truncation that unseals");
    appended_many(store, BASE + SEGMENT + 5..BASE + 3 * SEGMENT);
    assert_totals_current(store, "appends that seal again");
    let mut fork = store.clone();
    assert!(fork.set(row, key("level"), &Value::Int64(1), None));
    assert_totals_current(&fork, "a fork's write");
    assert_totals_current(store, "a fork's write, on the source");
}
