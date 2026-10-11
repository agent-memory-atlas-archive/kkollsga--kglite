//! Unit tests for the value model, split from `values.rs` to keep that file under its
//! size ceiling.
// Test data uses the literal `3.14` as a plain float, not an approximation of PI.
#![allow(clippy::approx_constant)]

use super::*;

#[test]
fn test_as_string_with_string_value() {
    let v = Value::String("hello".to_string());
    assert_eq!(v.as_string(), Some("hello".to_string()));
}

#[test]
fn test_timestamp_roundtrip_order_and_meta() {
    use chrono::{NaiveDate, NaiveDateTime};
    let dt: NaiveDateTime = NaiveDate::from_ymd_opt(2024, 3, 15)
        .unwrap()
        .and_hms_opt(10, 30, 45)
        .unwrap();
    let v = Value::Timestamp(dt);

    assert_eq!(v.type_name(), "Timestamp");
    assert_eq!(format_value(&v), "\"2024-03-15T10:30:45\"");

    // serde round-trip (the .kgl path for Mixed columns).
    let bytes =
        crate::serde_codec::encode_versioned(crate::serde_codec::CURRENT_CODEC, &v, u64::MAX)
            .unwrap();
    assert_eq!(
        crate::serde_codec::decode_exact_with::<Value>(
            crate::serde_codec::CURRENT_CODEC,
            &bytes,
            bytes.len() as u64,
            crate::serde_codec::DecodeLimits::new(u64::MAX, u64::MAX),
        )
        .unwrap(),
        v
    );

    // Ord's own `disc` table (not the serde discriminant) sorts
    // Timestamp last, so a date-only value orders before any timestamp.
    let date = Value::DateTime(NaiveDate::from_ymd_opt(2024, 3, 15).unwrap());
    assert!(date < v);

    let later = Value::Timestamp(
        NaiveDate::from_ymd_opt(2024, 3, 15)
            .unwrap()
            .and_hms_opt(10, 30, 46)
            .unwrap(),
    );
    assert!(v < later);
}

#[test]
fn test_as_string_with_non_string_values() {
    assert_eq!(Value::Int64(42).as_string(), None);
    assert_eq!(Value::Float64(3.14).as_string(), None);
    assert_eq!(Value::Boolean(true).as_string(), None);
    assert_eq!(Value::Null.as_string(), None);
    assert_eq!(Value::UniqueId(1).as_string(), None);
}

#[test]
fn test_value_equality_same_types() {
    assert_eq!(Value::Int64(42), Value::Int64(42));
    assert_eq!(Value::Float64(3.14), Value::Float64(3.14));
    assert_eq!(
        Value::String("a".to_string()),
        Value::String("a".to_string())
    );
    assert_eq!(Value::Boolean(true), Value::Boolean(true));
    assert_eq!(Value::Null, Value::Null);
    assert_eq!(Value::UniqueId(5), Value::UniqueId(5));
}

#[test]
fn test_value_inequality() {
    assert_ne!(Value::Int64(1), Value::Int64(2));
    assert_ne!(
        Value::String("a".to_string()),
        Value::String("b".to_string())
    );
    assert_ne!(Value::Boolean(true), Value::Boolean(false));
}

#[test]
fn test_value_hash_consistency() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(Value::Int64(42));
    set.insert(Value::Int64(42));
    assert_eq!(set.len(), 1);

    set.insert(Value::String("test".to_string()));
    assert_eq!(set.len(), 2);

    set.insert(Value::Null);
    set.insert(Value::Null);
    assert_eq!(set.len(), 3);
}

#[test]
fn test_float_hash_negative_zero() {
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(Value::Float64(0.0));
    set.insert(Value::Float64(-0.0));
    // 0.0 and -0.0 should hash the same
    assert_eq!(set.len(), 1);
}

#[test]
fn test_format_value_types() {
    assert_eq!(format_value(&Value::UniqueId(42)), "42");
    assert_eq!(format_value(&Value::Int64(-5)), "-5");
    assert_eq!(format_value(&Value::Float64(3.14)), "3.14");
    assert_eq!(format_value(&Value::String("hi".to_string())), "\"hi\"");
    assert_eq!(format_value(&Value::Boolean(true)), "true");
    assert_eq!(format_value(&Value::Null), "NULL");
}

#[test]
fn test_format_value_nan_is_null() {
    assert_eq!(format_value(&Value::Float64(f64::NAN)), "NULL");
}

#[test]
fn test_column_type_display() {
    assert_eq!(format!("{}", ColumnType::UniqueId), "UniqueId");
    assert_eq!(format!("{}", ColumnType::Int64), "Int64");
    assert_eq!(format!("{}", ColumnType::Float64), "Float64");
    assert_eq!(format!("{}", ColumnType::String), "String");
    assert_eq!(format!("{}", ColumnType::Boolean), "Boolean");
    assert_eq!(format!("{}", ColumnType::DateTime), "DateTime");
}

#[test]
fn test_dataframe_new_empty() {
    let df = DataFrame::new(vec![
        ("id".to_string(), ColumnType::Int64),
        ("name".to_string(), ColumnType::String),
    ]);
    assert_eq!(df.row_count(), 0);
    assert_eq!(df.column_count(), 2);
    assert!(df.verify_column("id"));
    assert!(df.verify_column("name"));
    assert!(!df.verify_column("missing"));
}

#[test]
fn test_dataframe_column_names() {
    let df = DataFrame::new(vec![
        ("a".to_string(), ColumnType::Int64),
        ("b".to_string(), ColumnType::String),
    ]);
    let names = df.get_column_names();
    assert_eq!(names, vec!["a", "b"]);
}

#[test]
fn test_dataframe_column_type() {
    let df = DataFrame::new(vec![
        ("id".to_string(), ColumnType::Int64),
        ("name".to_string(), ColumnType::String),
    ]);
    assert_eq!(df.get_column_type("id"), Some(ColumnType::Int64));
    assert_eq!(df.get_column_type("name"), Some(ColumnType::String));
    assert_eq!(df.get_column_type("missing"), None);
}

#[test]
fn test_dataframe_add_column() {
    let mut df = DataFrame::new(vec![("id".to_string(), ColumnType::Int64)]);
    let result = df.add_column(
        "name".to_string(),
        ColumnType::String,
        ColumnData::String(vec![]),
    );
    assert!(result.is_ok());
    assert_eq!(df.column_count(), 2);
}

#[test]
fn test_dataframe_add_duplicate_column() {
    let mut df = DataFrame::new(vec![("id".to_string(), ColumnType::Int64)]);
    let result = df.add_column(
        "id".to_string(),
        ColumnType::Int64,
        ColumnData::Int64(vec![]),
    );
    assert!(result.is_err());
}

#[test]
fn test_dataframe_add_column_type_mismatch() {
    let mut df = DataFrame::new(vec![]);
    let result = df.add_column(
        "x".to_string(),
        ColumnType::Int64,
        ColumnData::String(vec![]),
    );
    assert!(result.is_err());
}

#[test]
fn test_dataframe_add_column_length_mismatch() {
    let mut df = DataFrame::new(vec![]);
    df.add_column(
        "a".to_string(),
        ColumnType::Int64,
        ColumnData::Int64(vec![Some(1), Some(2)]),
    )
    .unwrap();
    let result = df.add_column(
        "b".to_string(),
        ColumnType::String,
        ColumnData::String(vec![Some("x".to_string())]),
    );
    assert!(result.is_err(), "non-rectangular add must fail");
    assert_eq!(df.column_count(), 1);
    df.add_column(
        "b".to_string(),
        ColumnType::String,
        ColumnData::String(vec![Some("x".to_string()), None]),
    )
    .unwrap();
    assert_eq!(df.row_count(), 2);
}

fn one_col(rows: Vec<Value>) -> DataFrame {
    DataFrame::from_cypher_rows(
        vec!["c".to_string()],
        rows.into_iter().map(|v| vec![v]).collect(),
    )
    .unwrap()
}

#[test]
fn test_promotion_int_then_float_becomes_float() {
    let df = one_col(vec![Value::Int64(1), Value::Float64(1.5)]);
    assert_eq!(df.get_column_type("c"), Some(ColumnType::Float64));
    assert_eq!(df.get_value(0, "c"), Some(Value::Float64(1.0)));
    assert_eq!(df.get_value(1, "c"), Some(Value::Float64(1.5)));
}

#[test]
fn test_promotion_float_then_int_becomes_float() {
    let df = one_col(vec![Value::Float64(2.5), Value::Int64(7)]);
    assert_eq!(df.get_column_type("c"), Some(ColumnType::Float64));
    assert_eq!(df.get_value(1, "c"), Some(Value::Float64(7.0)));
}

/// A float column holds an integer only when the float is that integer:
/// 2^53 + 1 would be stored as 2^53, so the mix takes the text fallback,
/// which writes every value exactly.
#[test]
fn test_promotion_float_with_an_inexact_int_keeps_every_value_exact() {
    let big = (1i64 << 53) + 1;
    let df = one_col(vec![Value::Float64(1.5), Value::Int64(big)]);
    assert_eq!(df.get_column_type("c"), Some(ColumnType::String));
    assert_eq!(df.get_value(0, "c"), Some(Value::String("1.5".into())));
    assert_eq!(df.get_value(1, "c"), Some(Value::String(big.to_string())));
    let exact = one_col(vec![Value::Float64(1.5), Value::Int64(1i64 << 53)]);
    assert_eq!(exact.get_column_type("c"), Some(ColumnType::Float64));
    assert_eq!(
        classify_value_set(&[Value::Float64(1.5), Value::Int64(big)]),
        ValueSetType::Mixed
    );
    assert_eq!(
        classify_value_set(&[Value::Float64(1.5), Value::Int64(3)]),
        ValueSetType::Uniform(ColumnType::Float64)
    );
}

#[test]
fn test_promotion_uniqueid_with_big_int64_widens_to_int64() {
    let big = u32::MAX as i64 + 1_000;
    let df = one_col(vec![Value::UniqueId(7), Value::Int64(big)]);
    assert_eq!(df.get_column_type("c"), Some(ColumnType::Int64));
    assert_eq!(df.get_value(0, "c"), Some(Value::Int64(7)));
    assert_eq!(df.get_value(1, "c"), Some(Value::Int64(big)));
}

#[test]
fn test_promotion_uniqueid_with_negative_int64_widens_to_int64() {
    let df = one_col(vec![Value::UniqueId(7), Value::Int64(-1)]);
    assert_eq!(df.get_column_type("c"), Some(ColumnType::Int64));
    assert_eq!(df.get_value(1, "c"), Some(Value::Int64(-1)));
}

#[test]
fn test_promotion_uniqueid_with_small_int64_stays_uniqueid() {
    let df = one_col(vec![Value::UniqueId(7), Value::Int64(42)]);
    assert_eq!(df.get_column_type("c"), Some(ColumnType::UniqueId));
    assert_eq!(df.get_value(1, "c"), Some(Value::UniqueId(42)));
}

#[test]
fn test_promotion_bool_and_int_becomes_string() {
    let df = one_col(vec![Value::Boolean(true), Value::Int64(3)]);
    assert_eq!(df.get_column_type("c"), Some(ColumnType::String));
    assert_eq!(df.get_value(0, "c"), Some(Value::String("true".into())));
    assert_eq!(df.get_value(1, "c"), Some(Value::String("3".into())));
}

#[test]
fn test_promotion_all_null_defaults_to_int64() {
    let df = one_col(vec![Value::Null, Value::Null]);
    assert_eq!(df.get_column_type("c"), Some(ColumnType::Int64));
    assert_eq!(df.get_value(0, "c"), None);
}

#[test]
fn test_promotion_null_interleaved_does_not_disturb_type() {
    let df = one_col(vec![Value::Null, Value::Int64(1), Value::Float64(0.5)]);
    assert_eq!(df.get_column_type("c"), Some(ColumnType::Float64));
    assert_eq!(df.get_value(0, "c"), None);
    assert_eq!(df.get_value(2, "c"), Some(Value::Float64(0.5)));
}

#[test]
fn test_promotion_list_mix_wraps_scalars() {
    let df = one_col(vec![
        Value::Int64(5),
        Value::List(vec![Value::Int64(1), Value::Int64(2)]),
    ]);
    assert_eq!(df.get_column_type("c"), Some(ColumnType::List));
    assert_eq!(
        df.get_value(0, "c"),
        Some(Value::List(vec![Value::Int64(5)]))
    );
}

#[test]
fn test_promotion_date_and_timestamp_becomes_timestamp() {
    use chrono::NaiveDate;
    let date = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap();
    let ts = date.and_hms_opt(10, 30, 45).unwrap();
    let df = one_col(vec![Value::DateTime(date), Value::Timestamp(ts)]);
    assert_eq!(df.get_column_type("c"), Some(ColumnType::Timestamp));
    assert_eq!(
        df.get_value(0, "c"),
        Some(Value::Timestamp(date.and_hms_opt(0, 0, 0).unwrap()))
    );
    assert_eq!(df.get_value(1, "c"), Some(Value::Timestamp(ts)));
}

#[test]
fn test_promotion_map_and_scalar_becomes_string() {
    let mut m = std::collections::BTreeMap::new();
    m.insert("k".to_string(), Value::Int64(1));
    let df = one_col(vec![Value::Map(m.into()), Value::Int64(2)]);
    assert_eq!(df.get_column_type("c"), Some(ColumnType::String));
    assert_eq!(df.get_value(0, "c"), Some(Value::String("{k: 1}".into())));
    assert_eq!(df.get_value(1, "c"), Some(Value::String("2".into())));
}

#[test]
fn test_timestamp_column_roundtrip() {
    use chrono::NaiveDate;
    let ts = NaiveDate::from_ymd_opt(2024, 3, 15)
        .unwrap()
        .and_hms_opt(10, 30, 45)
        .unwrap();
    let df = one_col(vec![Value::Timestamp(ts), Value::Null]);
    assert_eq!(df.get_column_type("c"), Some(ColumnType::Timestamp));
    assert_eq!(df.get_value(0, "c"), Some(Value::Timestamp(ts)));
    assert_eq!(df.get_value(1, "c"), None);
}

/// Every persistable Value variant, fed as a single-value column, must
/// come back non-null: no inference arm may point at a fill arm that
/// drops it (`Null` and the internal `NodeRef` are the only variants
/// allowed to read back as None).
#[test]
fn test_every_variant_survives_single_column() {
    use chrono::NaiveDate;
    let date = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap();
    let node = NodeValue {
        id: 1,
        labels: vec!["L".to_string()],
        properties: PropMap::new(),
    };
    let rel = RelValue {
        incarnation: None,
        id: 1,
        start_id: 1,
        end_id: 2,
        rel_type: "R".to_string(),
        properties: PropMap::new(),
    };
    let representatives = vec![
        Value::UniqueId(1),
        Value::Int64(-5),
        Value::Float64(1.5),
        Value::String("s".to_string()),
        Value::Boolean(false),
        Value::DateTime(date),
        Value::Timestamp(date.and_hms_opt(1, 2, 3).unwrap()),
        Value::Point { lat: 1.0, lon: 2.0 },
        Value::Duration {
            months: 1,
            days: 2,
            seconds: 3,
            nanos: 0,
        },
        Value::List(vec![Value::Int64(1)]),
        Value::Map(PropMap::new()),
        Value::Node(Box::new(node)),
        Value::Relationship(Box::new(rel.clone())),
        Value::Path(Box::new(PathValue {
            nodes: vec![],
            rels: vec![rel],
        })),
    ];
    for val in representatives {
        let name = val.type_name();
        let df = one_col(vec![val]);
        let got = df.get_value(0, "c");
        assert!(
            !matches!(got, None | Some(Value::Null)),
            "single-column {} value read back as null (got {:?})",
            name,
            got
        );
    }
}

#[test]
fn test_from_cypher_rows_empty_defaults_int64() {
    let df = DataFrame::from_cypher_rows(vec!["a".to_string(), "b".to_string()], vec![]).unwrap();
    assert_eq!(df.row_count(), 0);
    assert_eq!(df.get_column_type("a"), Some(ColumnType::Int64));
    assert_eq!(df.get_column_type("b"), Some(ColumnType::Int64));
}

#[test]
fn test_dataframe_get_column_index() {
    let df = DataFrame::new(vec![
        ("a".to_string(), ColumnType::Int64),
        ("b".to_string(), ColumnType::String),
    ]);
    assert_eq!(df.get_column_index("a"), Some(0));
    assert_eq!(df.get_column_index("b"), Some(1));
    assert_eq!(df.get_column_index("c"), None);
}
