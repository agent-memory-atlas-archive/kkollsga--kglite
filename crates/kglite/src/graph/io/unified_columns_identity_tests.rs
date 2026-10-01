use super::*;
use crate::datatypes::values::{BorrowedValue, Value};
use crate::graph::schema::StringInterner;
use crate::graph::schema::TypeSchema;
use memmap2::{MmapMut, MmapOptions};
use std::fs::File;

fn store(ids: &[Value]) -> ColumnStore {
    let mut store = ColumnStore::new(
        Arc::new(TypeSchema::new()),
        &HashMap::new(),
        &StringInterner::new(),
    );
    for id in ids {
        store.push_id(id);
        store.push_title(&Value::String("row".into()));
        store.push_row(&[]);
    }
    store
}

fn mapped(store: ColumnStore) -> (ColumnTypeMeta, MmapMut) {
    let dir = tempfile::tempdir().unwrap();
    // The extra type supplies bytes when the tested column has zero rows.
    let stores = HashMap::from([
        ("Subject".into(), Arc::new(store)),
        (
            "Control".into(),
            Arc::new(self::store(&[Value::UniqueId(7)])),
        ),
    ]);
    let result = write_unified_columns_published(dir.path(), &stores, None).unwrap();
    assert!(result.files.contains_key("Subject"));
    type_file(dir.path(), "Subject")
}

/// One type's metadata and a private copy-on-write map of its own file.
fn type_file(dir: &std::path::Path, type_name: &str) -> (ColumnTypeMeta, MmapMut) {
    let seg0 = dir.join("seg_000");
    let mut meta = crate::graph::io::columns_meta::read(&seg0.join("columns_meta.json")).unwrap();
    let relative = meta
        .files
        .remove(type_name)
        .expect("the type has its own file");
    let file = File::open(seg0.join(relative)).unwrap();
    // SAFETY: the test owns this immutable file; the private map survives its unlink.
    let mmap = unsafe { MmapOptions::new().map_copy(&file).unwrap() };
    (
        meta.types
            .into_iter()
            .find(|m| m.type_name == type_name)
            .unwrap(),
        mmap,
    )
}

fn read_only(mmap: MmapMut) -> Arc<memmap2::Mmap> {
    Arc::new(mmap.make_read_only().unwrap())
}

#[test]
fn unified_integer_identity_reads_preserve_width_sign_nulls_and_borrowing() {
    let values = [
        Value::Int64(i64::MIN),
        Value::Int64(-1),
        Value::Null,
        Value::Int64(i32::MAX as i64 + 1),
        Value::Int64(u32::MAX as i64 + 1),
        Value::Int64(i64::MAX),
    ];
    let (meta, mmap) = mapped(store(&values));
    assert_eq!(meta.id_data.len, values.len() * 8);
    let loaded = ColumnStore::from_mmap_store(Arc::new(meta.to_mmap_store(read_only(mmap))));
    assert_eq!(loaded.id_type_str(), Some("int64"));
    for (i, expected) in values.iter().enumerate() {
        let expected = (!matches!(expected, Value::Null)).then_some(expected.clone());
        assert_eq!(loaded.get_id(i as u32), expected);
        assert_eq!(
            loaded.id_borrowed(i as u32).map(BorrowedValue::to_value),
            expected
        );
        if expected.is_some() {
            assert!(matches!(
                loaded.id_borrowed(i as u32),
                Some(BorrowedValue::Int64(_))
            ));
        }
    }
}

#[test]
fn unified_compact_ids_keep_native_u32_representation() {
    let values = [Value::UniqueId(0), Value::UniqueId(u32::MAX), Value::Null];
    let (meta, mmap) = mapped(store(&values));
    assert_eq!(meta.id_data.len, values.len() * 4);
    let loaded = ColumnStore::from_mmap_store(Arc::new(meta.to_mmap_store(read_only(mmap))));
    assert_eq!(loaded.id_type_str(), Some("uniqueid"));
    assert!(matches!(loaded.get_id(1), Some(Value::UniqueId(u32::MAX))));
    assert!(matches!(
        loaded.id_borrowed(1),
        Some(BorrowedValue::UniqueId(u32::MAX))
    ));
    assert!(loaded.get_id(2).is_none());
}

#[test]
fn empty_missing_and_all_null_fixed_ids_never_create_zero_identities() {
    let mut empty = store(&[Value::Int64(1)]);
    empty.truncate_rows(0);
    let (meta, mmap) = mapped(empty);
    assert_eq!(meta.id_data.len, 0);
    let loaded = meta.to_mmap_store(read_only(mmap));
    assert!(loaded.get_id(0).is_none());
    assert!(loaded.id_borrowed(0).is_none());

    let mut missing = store(&[]);
    missing.push_title(&Value::String("no id".into()));
    missing.push_row(&[]);
    let (meta, mmap) = mapped(missing);
    let loaded = meta.to_mmap_store(read_only(mmap));
    assert!(loaded.get_id(0).is_none());
    assert!(loaded.id_borrowed(0).is_none());

    let (meta, mut mmap) = mapped(store(&[Value::Int64(1), Value::Int64(2)]));
    // Valid fixed-width payload with every row marked null still carries its width.
    mmap[meta.id_nulls.offset..meta.id_nulls.offset + meta.id_nulls.len].fill(1);
    let loaded = meta.to_mmap_store(read_only(mmap));
    for row in 0..2 {
        assert!(loaded.get_id(row).is_none());
        assert!(loaded.id_borrowed(row).is_none());
    }
}

#[test]
fn unsupported_identity_columns_require_lossless_sidecars() {
    for value in [
        Value::Float64(1.5),
        Value::Boolean(true),
        Value::DateTime(chrono::NaiveDate::from_ymd_opt(2020, 1, 1).unwrap()),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let stores = HashMap::from([("Subject".into(), Arc::new(store(&[value])))]);
        let result = write_unified_columns_published(dir.path(), &stores, None).unwrap();
        assert!(result.files.is_empty(), "no type landed in a file");
        assert!(!dir.path().join("seg_000/type_columns").exists());
    }
}

/// A `Timestamp` column is typed, saves into both the packed sidecar and
/// its type file, and reads back as the same `Value::Timestamp`; a value it
/// cannot hold exactly is refused by the column, never truncated.
#[test]
fn timestamp_column_round_trips_through_packed_and_unified_layouts() {
    use chrono::NaiveDate;
    let mut interner = StringInterner::new();
    let ts_key = interner.get_or_intern("rec_from");
    let schema = Arc::new(TypeSchema::from_keys(vec![ts_key]));
    let meta = HashMap::from([("rec_from".to_string(), "Timestamp".to_string())]);
    let t1 = NaiveDate::from_ymd_opt(2009, 11, 6)
        .unwrap()
        .and_hms_micro_opt(12, 0, 0, 123_456)
        .unwrap();
    let t0 = NaiveDate::from_ymd_opt(1601, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let mut store = ColumnStore::new(schema.clone(), &meta, &interner);
    for v in [Value::Timestamp(t1), Value::Null, Value::Timestamp(t0)] {
        store.push_id(&Value::Int64(store.row_count() as i64));
        store.push_title(&Value::String("row".into()));
        store.push_row(&[(ts_key, v)]);
    }
    assert!(matches!(
        store.column(0),
        Some(TypedColumn::Timestamp { .. })
    ));
    assert_eq!(store.column_type_str(0), Some("timestamp"));

    // packed sidecar codec
    let packed = store.write_packed(&interner).unwrap();
    let loaded =
        ColumnStore::load_packed(schema.clone(), &meta, &interner, &packed, 3, None).unwrap();
    assert!(matches!(
        loaded.column(0),
        Some(TypedColumn::Timestamp { .. })
    ));
    assert_eq!(loaded.get(0, ts_key), Some(Value::Timestamp(t1)));
    assert_eq!(loaded.get(1, ts_key), None);
    assert_eq!(loaded.get(2, ts_key), Some(Value::Timestamp(t0)));

    // per-type file (mmap) layout
    let dir = tempfile::tempdir().unwrap();
    let stores = HashMap::from([("T".to_string(), Arc::new(store))]);
    let result = write_unified_columns_published(dir.path(), &stores, None).unwrap();
    assert!(
        result.files.contains_key("T"),
        "a Timestamp column must not force a sidecar"
    );
    let (type_meta, mmap) = type_file(dir.path(), "T");
    let mapped = ColumnStore::from_mmap_store(Arc::new(type_meta.to_mmap_store(read_only(mmap))));
    assert_eq!(mapped.get(0, ts_key), Some(Value::Timestamp(t1)));
    assert_eq!(mapped.get(1, ts_key), None);
    assert_eq!(mapped.get(2, ts_key), Some(Value::Timestamp(t0)));

    // sub-microsecond precision and leap seconds are refused, not truncated
    let mut col = TypedColumn::from_type_str("timestamp");
    let ns = t1 + chrono::Duration::nanoseconds(1);
    assert!(col.push(&Value::Timestamp(ns)).is_err());
    let leap = chrono::NaiveDate::from_ymd_opt(2016, 12, 31)
        .unwrap()
        .and_hms_nano_opt(23, 59, 59, 1_500_000_000)
        .unwrap();
    assert!(col.push(&Value::Timestamp(leap)).is_err());
}

/// A file of a generation is written once: a second write into the same stage
/// is an error, never a truncation of bytes another generation may share.
#[test]
fn a_type_file_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let stores = HashMap::from([("Subject".to_string(), Arc::new(store(&[Value::Int64(1)])))]);
    write_unified_columns_published(dir.path(), &stores, None).unwrap();
    let relative =
        crate::graph::io::columns_meta::read(&dir.path().join("seg_000/columns_meta.json"))
            .unwrap()
            .files["Subject"]
            .clone();
    let file = dir.path().join("seg_000").join(&relative);
    let before = std::fs::read(&file).unwrap();
    let error = write_unified_columns_published(dir.path(), &stores, None)
        .err()
        .expect("the second write must not replace the published file");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists, "{error}");
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

/// A type with no rows still gets a file (a zero-length file cannot be mapped),
/// and it loads as an empty store.
#[test]
fn an_empty_type_gets_a_mappable_file() {
    let mut empty = store(&[Value::Int64(1)]);
    empty.truncate_rows(0);
    let dir = tempfile::tempdir().unwrap();
    let stores = HashMap::from([("Empty".to_string(), Arc::new(empty))]);
    let result = write_unified_columns_published(dir.path(), &stores, None).unwrap();
    assert!(result.files.contains_key("Empty"));
    let (meta, mmap) = type_file(dir.path(), "Empty");
    assert_eq!(mmap.len(), 1);
    let loaded = ColumnStore::from_mmap_store(Arc::new(meta.to_mmap_store(read_only(mmap))));
    assert_eq!(loaded.row_count(), 0);
}

/// A save holds one type's plan, and so one type's owned buffers, at a time: its
/// peak heap is the largest touched type's, not the sum of them.
#[test]
fn a_write_holds_one_types_plan_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let stores: HashMap<String, Arc<ColumnStore>> = ["A", "B", "C", "D"]
        .into_iter()
        .map(|name| (name.to_string(), Arc::new(store(&[Value::Int64(1)]))))
        .collect();
    reset_live_plans();
    let meta = write_unified_columns(dir.path(), &stores, None).unwrap();
    assert_eq!(meta.files.len(), 4, "every type landed in a file");
    assert_eq!(
        peak_live_plans(),
        1,
        "each plan must be dropped before the next type is planned"
    );
}
