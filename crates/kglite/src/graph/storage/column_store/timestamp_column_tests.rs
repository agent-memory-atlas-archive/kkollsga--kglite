//! The typed `Timestamp` column holds only what it can hold exactly: a value
//! finer than a microsecond, or on a leap second, is classified `mixed` where
//! the write sites choose a column type, so a row is never dropped and a later
//! row never reads one slot off.
use super::*;
use chrono::{NaiveDate, NaiveDateTime};

fn ts(h: u32, m: u32, s: u32, micro: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2009, 6, 30)
        .unwrap()
        .and_hms_micro_opt(h, m, s, micro)
        .unwrap()
}

/// 12:00:00.000000123 — a Linux `localdatetime()` carries nanoseconds.
fn sub_micro() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2009, 6, 30)
        .unwrap()
        .and_hms_nano_opt(12, 0, 0, 123)
        .unwrap()
}

/// 23:59:59 plus 1.5 s: chrono's leap-second form, `nanosecond() >= 1e9`.
fn leap_second() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2016, 12, 31)
        .unwrap()
        .and_hms_nano_opt(23, 59, 59, 1_500_000_000)
        .unwrap()
}

fn store_for(declared: &[(&str, &str)]) -> (ColumnStore, InternedKey, StringInterner) {
    let mut interner = StringInterner::new();
    let key = interner.get_or_intern("t");
    let meta: HashMap<String, String> = declared
        .iter()
        .map(|(k, t)| (k.to_string(), t.to_string()))
        .collect();
    // An undeclared key starts with no column, so the first value types it.
    let schema = if declared.is_empty() {
        TypeSchema::new()
    } else {
        TypeSchema::from_keys([key])
    };
    let store = ColumnStore::new(Arc::new(schema), &meta, &interner);
    (store, key, interner)
}

fn push(store: &mut ColumnStore, key: InternedKey, value: Value) {
    store.push_id(&Value::Int64(store.row_count() as i64));
    store.push_title(&Value::Null);
    store.push_row(&[(key, value)]);
}

fn read(store: &ColumnStore, key: InternedKey) -> Vec<Option<Value>> {
    (0..store.row_count()).map(|r| store.get(r, key)).collect()
}

fn ts_value(t: NaiveDateTime) -> Option<Value> {
    Some(Value::Timestamp(t))
}

#[test]
fn only_exactly_encodable_timestamps_classify_as_timestamp() {
    let kind = |t| TypedColumn::type_str_for_value(&Value::Timestamp(t));
    assert_eq!(kind(ts(12, 0, 0, 0)), "timestamp");
    assert_eq!(kind(ts(12, 0, 0, 999_999)), "timestamp");
    assert_eq!(kind(sub_micro()), "mixed");
    assert_eq!(kind(leap_second()), "mixed");
    // The encoder and the classification agree, and the endpoint index reads
    // the same encoder.
    for t in [ts(1, 2, 3, 4), sub_micro(), leap_second()] {
        assert_eq!(exact_micros(t).is_some(), kind(t) == "timestamp", "{t:?}");
    }
}

/// The spike typed a null-first column `Timestamp` for a sub-microsecond
/// value, the push then failed, and every later row read one slot off.
#[test]
fn a_sub_micro_value_after_a_null_first_row_keeps_rows_aligned() {
    let (mut store, key, _) = store_for(&[]);
    let rows = [
        Value::Null,
        Value::Timestamp(sub_micro()),
        Value::Timestamp(ts(1, 0, 0, 5)),
        Value::Timestamp(ts(2, 0, 0, 6)),
    ];
    for v in &rows {
        push(&mut store, key, v.clone());
    }
    assert_eq!(
        read(&store, key),
        vec![
            None,
            ts_value(sub_micro()),
            ts_value(ts(1, 0, 0, 5)),
            ts_value(ts(2, 0, 0, 6))
        ]
    );
    assert_eq!(store.column(0).unwrap().len(), store.row_count() as usize);
}

#[test]
fn a_sub_micro_value_into_a_declared_all_null_column_keeps_rows_aligned() {
    let (mut store, key, _) = store_for(&[("t", "Timestamp")]);
    assert!(matches!(
        store.column(0),
        Some(TypedColumn::Timestamp { .. })
    ));
    push(&mut store, key, Value::Null);
    push(&mut store, key, Value::Timestamp(sub_micro()));
    push(&mut store, key, Value::Timestamp(ts(3, 0, 0, 7)));
    assert_eq!(
        read(&store, key),
        vec![None, ts_value(sub_micro()), ts_value(ts(3, 0, 0, 7))]
    );
    assert_eq!(store.column(0).unwrap().len(), 3);
    assert_eq!(store.column_type_str(0), Some("mixed"));
}

#[test]
fn a_leap_second_is_kept_not_moved_to_the_next_second() {
    let (mut store, key, _) = store_for(&[("t", "Timestamp")]);
    push(&mut store, key, Value::Timestamp(leap_second()));
    push(
        &mut store,
        key,
        Value::Timestamp(
            NaiveDate::from_ymd_opt(2017, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        ),
    );
    assert_eq!(read(&store, key)[0], ts_value(leap_second()));
    assert_ne!(read(&store, key)[0], read(&store, key)[1]);
}

#[test]
fn exact_values_stay_in_a_typed_column() {
    let (mut store, key, _) = store_for(&[]);
    push(&mut store, key, Value::Timestamp(ts(1, 0, 0, 1)));
    push(&mut store, key, Value::Null);
    push(&mut store, key, Value::Timestamp(ts(2, 0, 0, 2)));
    assert!(matches!(
        store.column(0),
        Some(TypedColumn::Timestamp { .. })
    ));
    assert_eq!(
        read(&store, key),
        vec![ts_value(ts(1, 0, 0, 1)), None, ts_value(ts(2, 0, 0, 2))]
    );
}

/// A `SET` of a value the column cannot hold widens the column instead of
/// dropping the value while reporting success — into an all-null column (the
/// spike retyped it to another `Timestamp` and lost the write) and into one
/// that already holds values.
#[test]
fn set_widens_instead_of_dropping_a_value_the_column_cannot_hold() {
    for prefill in [false, true] {
        let (mut store, key, _) = store_for(&[("t", "Timestamp")]);
        for r in 0..3 {
            let v = if prefill {
                Value::Timestamp(ts(r, 0, 0, 1))
            } else {
                Value::Null
            };
            push(&mut store, key, v);
        }
        assert!(store.set(1, key, &Value::Timestamp(sub_micro()), None));
        assert_eq!(
            store.get(1, key),
            ts_value(sub_micro()),
            "prefill={prefill}"
        );
        assert_eq!(
            store.get(0, key),
            prefill.then(|| Value::Timestamp(ts(0, 0, 0, 1)))
        );
        assert_eq!(
            store.get(2, key),
            prefill.then(|| Value::Timestamp(ts(2, 0, 0, 1)))
        );
        assert!(store.set(0, key, &Value::Timestamp(leap_second()), None));
        assert_eq!(store.get(0, key), ts_value(leap_second()));
    }
}

#[test]
fn set_at_slot_of_an_exact_value_stays_typed() {
    let (mut store, key, _) = store_for(&[("t", "Timestamp")]);
    push(&mut store, key, Value::Null);
    let slot = store.slot(key).unwrap();
    assert!(store.set_at_slot(0, slot, &Value::Timestamp(ts(9, 0, 0, 9))));
    assert_eq!(store.get(0, key), ts_value(ts(9, 0, 0, 9)));
    assert!(matches!(
        store.column(0),
        Some(TypedColumn::Timestamp { .. })
    ));
}

#[test]
fn truncate_rows_and_a_repush_keep_the_column_aligned() {
    let (mut store, key, _) = store_for(&[("t", "Timestamp")]);
    for r in 0..4 {
        push(&mut store, key, Value::Timestamp(ts(r, 0, 0, r)));
    }
    store.truncate_rows(2);
    assert_eq!(store.column(0).unwrap().len(), 2);
    push(&mut store, key, Value::Timestamp(ts(20, 0, 0, 20)));
    assert_eq!(
        read(&store, key),
        vec![
            ts_value(ts(0, 0, 0, 0)),
            ts_value(ts(1, 0, 0, 1)),
            ts_value(ts(20, 0, 0, 20))
        ]
    );
}

#[test]
fn gather_keeps_timestamps_and_nulls() {
    let (mut store, key, _) = store_for(&[("t", "Timestamp")]);
    push(&mut store, key, Value::Timestamp(ts(1, 0, 0, 1)));
    push(&mut store, key, Value::Null);
    push(&mut store, key, Value::Timestamp(ts(3, 0, 0, 3)));
    let gathered = store.gather_rows(&[2, 1, 0]).unwrap();
    assert!(matches!(
        gathered.column(0),
        Some(TypedColumn::Timestamp { .. })
    ));
    assert_eq!(
        read(&gathered, key),
        vec![ts_value(ts(3, 0, 0, 3)), None, ts_value(ts(1, 0, 0, 1))]
    );
}

#[test]
fn packed_round_trip_keeps_typed_and_widened_columns() {
    for (values, kind) in [
        (
            vec![
                Value::Timestamp(ts(1, 0, 0, 1)),
                Value::Null,
                Value::Timestamp(ts(2, 0, 0, 2)),
            ],
            "timestamp",
        ),
        (
            vec![
                Value::Null,
                Value::Timestamp(sub_micro()),
                Value::Timestamp(ts(2, 0, 0, 2)),
            ],
            "mixed",
        ),
    ] {
        let (mut store, key, interner) = store_for(&[]);
        for v in &values {
            push(&mut store, key, v.clone());
        }
        assert_eq!(store.column_type_str(0), Some(kind));
        let packed = store.write_packed(&interner).unwrap();
        let schema = store.schema.clone();
        let loaded = ColumnStore::load_packed(
            schema,
            &HashMap::new(),
            &interner,
            &packed,
            values.len() as u32,
            None,
        )
        .unwrap();
        assert_eq!(loaded.column_type_str(0), Some(kind));
        assert_eq!(read(&loaded, key), read(&store, key));
    }
}

/// A `Mixed` timestamp column (what 0.19.0 wrote) becomes a typed one on the
/// first disk save when every value is exactly encodable, and stays `Mixed`
/// when any is not.
#[test]
fn an_old_mixed_timestamp_column_retypes_only_when_every_value_is_exact() {
    for (values, retypes) in [
        (vec![ts(1, 0, 0, 1), ts(2, 0, 0, 2)], true),
        (vec![ts(1, 0, 0, 1), sub_micro()], false),
        (vec![leap_second()], false),
    ] {
        let mut interner = StringInterner::new();
        let key = interner.get_or_intern("t");
        let declared: HashMap<String, String> = [("t".to_string(), "mixed".to_string())].into();
        let mut store =
            ColumnStore::new(Arc::new(TypeSchema::from_keys([key])), &declared, &interner);
        for t in &values {
            push(&mut store, key, Value::Timestamp(*t));
        }
        assert_eq!(store.column_type_str(0), Some("mixed"));
        assert_eq!(store.has_retypable_mixed_column(&HashMap::new()), retypes);
        let flat = store.flattened_owned(&HashMap::new(), &interner);
        assert_eq!(
            flat.column_type_str(0),
            Some(if retypes { "timestamp" } else { "mixed" })
        );
        assert_eq!(read(&flat, key), read(&store, key));
    }
}

#[test]
fn timestamp_cells_read_the_column_as_epoch_micros() {
    let (mut store, key, _) = store_for(&[("t", "Timestamp")]);
    push(&mut store, key, Value::Timestamp(ts(0, 0, 0, 5)));
    push(&mut store, key, Value::Null);
    push(&mut store, key, Value::Timestamp(ts(0, 0, 1, 0)));
    store.tombstone(2);
    let cells = store.timestamp_cells(key).unwrap();
    assert_eq!(cells.micros(0), exact_micros(ts(0, 0, 0, 5)));
    assert_eq!(cells.micros(1), None, "NULL");
    assert_eq!(cells.micros(2), None, "tombstoned");
    assert_eq!(cells.micros(3), None, "past the end");
    // Another column kind answers for the key: no cells.
    let (mut other, k, _) = store_for(&[("t", "int64")]);
    push(&mut other, k, Value::Int64(1));
    assert!(other.timestamp_cells(k).is_none());
}

/// The decode is chrono's `from_timestamp_micros`, written out to skip its
/// wrapper: same value, and `None` where chrono has none.
#[test]
fn micros_decode_agrees_with_chrono_across_the_range() {
    const DAY: i64 = 86_400_000_000;
    let mut cases = vec![
        0,
        1,
        -1,
        DAY - 1,
        DAY,
        DAY + 1,
        -DAY,
        -DAY - 1,
        i64::MIN,
        i64::MAX,
    ];
    cases.extend([
        1_000_000,
        -1_000_000,
        999_999,
        -999_999,
        1_700_000_000_123_456,
    ]);
    // The extremes of chrono's range and one step past each.
    for edge in [chrono::NaiveDateTime::MIN, chrono::NaiveDateTime::MAX] {
        let micros = edge.and_utc().timestamp_micros();
        cases.extend([micros - 1, micros, micros + 1]);
    }
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..20_000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        cases.push(x as i64 >> (x % 20));
    }
    for micros in cases {
        let want = chrono::DateTime::from_timestamp_micros(micros).map(|d| d.naive_utc());
        assert_eq!(micros_to_timestamp(micros), want, "{micros}");
    }
}
