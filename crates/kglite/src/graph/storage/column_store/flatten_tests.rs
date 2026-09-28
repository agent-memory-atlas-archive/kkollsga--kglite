//! A flattened copy keeps every value's kind: it types a column from the
//! declared metadata only when the column's values already have that kind.
use super::*;

fn store_with(
    values: &[(&str, Value)],
    declared: &[(&str, &str)],
) -> (ColumnStore, HashMap<String, String>, StringInterner) {
    let mut interner = StringInterner::new();
    let mut store = ColumnStore::new(Arc::new(TypeSchema::new()), &HashMap::new(), &interner);
    for (row, (key, value)) in values.iter().enumerate() {
        let key = interner.get_or_intern(key);
        store.push_id(&Value::Int64(row as i64));
        store.push_title(&Value::Null);
        store.push_row(&[(key, value.clone())]);
    }
    let meta = declared
        .iter()
        .map(|(k, t)| (k.to_string(), t.to_string()))
        .collect();
    (store, meta, interner)
}

fn values(store: &ColumnStore, key: &str) -> Vec<Option<Value>> {
    let key = InternedKey::from_str(key);
    (0..store.row_count())
        .map(|row| store.get(row, key))
        .collect()
}

/// A column the type declares float but which holds an exact integer beside a
/// float (a `SET` widened the declaration) must not store the integer as a
/// float: a Float64 column accepts an Int64 by converting it.
#[test]
fn a_mixed_int_and_float_column_keeps_the_int() {
    let (store, meta, interner) = store_with(
        &[("v", Value::Int64(7)), ("v", Value::Float64(2.5))],
        &[("v", "float64")],
    );
    let flat = store.flattened_owned(&meta, &interner);
    assert_eq!(
        values(&flat, "v"),
        vec![Some(Value::Int64(7)), Some(Value::Float64(2.5))]
    );
}

/// Every other mixture keeps its values too, and a column whose values all
/// have one kind is typed by that kind, whatever the declaration names.
#[test]
fn a_column_is_typed_only_by_a_kind_its_values_already_have() {
    let date = Value::DateTime(chrono::NaiveDate::from_ymd_opt(2020, 1, 2).unwrap());
    for (pair, declared) in [
        ((Value::Boolean(true), Value::Int64(3)), "int64"),
        ((Value::Int64(4), Value::String("four".into())), "string"),
        ((date.clone(), Value::String("2020-01-02".into())), "date"),
    ] {
        let (store, meta, interner) = store_with(
            &[("v", pair.0.clone()), ("v", pair.1.clone())],
            &[("v", declared)],
        );
        let flat = store.flattened_owned(&meta, &interner);
        assert_eq!(
            values(&flat, "v"),
            vec![Some(pair.0), Some(pair.1)],
            "declared {declared}"
        );
    }

    let (store, meta, interner) = store_with(
        &[("v", Value::Int64(1)), ("v", Value::Int64(2))],
        &[("v", "float64")],
    );
    let flat = store.flattened_owned(&meta, &interner);
    assert_eq!(flat.column_type_str(0), Some("int64"));
    assert_eq!(
        values(&flat, "v"),
        vec![Some(Value::Int64(1)), Some(Value::Int64(2))]
    );

    let (store, meta, interner) = store_with(
        &[("v", Value::Float64(1.0)), ("v", Value::Float64(2.5))],
        &[("v", "float64")],
    );
    assert_eq!(
        store.flattened_owned(&meta, &interner).column_type_str(0),
        Some("float64")
    );
}
