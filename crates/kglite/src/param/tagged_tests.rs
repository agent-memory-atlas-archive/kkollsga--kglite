use super::super::*;
use chrono::NaiveDate;
use std::collections::HashMap;

fn params(source: &str) -> HashMap<String, Value> {
    json_text_to_query_value_map(source).unwrap()
}

fn rejection(source: &str) -> JsonQueryParameterError {
    match json_text_to_query_value_map(source) {
        Err(JsonQueryTextError::Parameter(error)) => error,
        other => panic!("{source} must be rejected as a parameter, got {other:?}"),
    }
}

#[test]
fn date_tag_becomes_a_date_value() {
    let params = params(r#"{"v":{"$date":"2020-01-01"}}"#);
    assert_eq!(
        params["v"],
        Value::DateTime(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap())
    );
}

#[test]
fn datetime_tag_becomes_a_timestamp_normalised_to_utc() {
    let params = params(
        r#"{"naive":{"$datetime":"2020-01-01T08:00:00.250"},"zoned":{"$datetime":"2020-01-01T10:00:00+02:00"}}"#,
    );
    let expected = NaiveDate::from_ymd_opt(2020, 1, 1)
        .unwrap()
        .and_hms_milli_opt(8, 0, 0, 250)
        .unwrap();
    assert_eq!(params["naive"], Value::Timestamp(expected));
    assert_eq!(
        params["zoned"],
        Value::Timestamp(expected - chrono::Duration::milliseconds(250))
    );
}

#[test]
fn duration_tag_takes_the_result_encoding_object() {
    let params = params(
        r#"{"v":{"$duration":{"months":1,"days":2,"seconds":3}},"w":{"$duration":{"days":7}}}"#,
    );
    assert_eq!(
        params["v"],
        Value::Duration {
            months: 1,
            days: 2,
            seconds: 3
        }
    );
    assert_eq!(
        params["w"],
        Value::Duration {
            months: 0,
            days: 7,
            seconds: 0
        }
    );
}

/// A result cell rendered by `kglite_value_to_json`, wrapped in its tag, reads
/// back as the value it was rendered from.
#[test]
fn result_encoding_round_trips_through_the_tag() {
    let date = NaiveDate::from_ymd_opt(1999, 12, 31).unwrap();
    let values = [
        ("$date", Value::DateTime(date)),
        (
            "$datetime",
            Value::Timestamp(date.and_hms_micro_opt(23, 59, 58, 123_456).unwrap()),
        ),
        (
            "$duration",
            Value::Duration {
                months: -2,
                days: 40,
                seconds: -7,
            },
        ),
    ];
    for (tag, value) in values {
        let source = serde_json::json!({ "v": { tag: kglite_value_to_json(&value) } }).to_string();
        assert_eq!(params(&source)["v"], value, "{source}");
    }
}

#[test]
fn tags_apply_inside_lists_and_maps() {
    let params = params(r#"{"v":[{"$date":"2020-01-01"},{"at":{"$date":"2021-02-03"}}]}"#);
    let Value::List(items) = &params["v"] else {
        panic!("list expected")
    };
    assert!(matches!(items[0], Value::DateTime(_)));
    let Value::Map(map) = &items[1] else {
        panic!("map expected")
    };
    assert!(matches!(map.get("at"), Some(Value::DateTime(_))));
}

#[test]
fn a_tag_key_beside_other_keys_is_an_ordinary_map() {
    let params = params(r#"{"v":{"$date":"2020-01-01","other":1},"w":{"date":"2020-01-01"}}"#);
    assert!(matches!(params["v"], Value::Map(_)));
    assert!(matches!(params["w"], Value::Map(_)));
}

#[test]
fn malformed_tags_are_rejected_with_their_path() {
    for (source, path) in [
        (r#"{"v":{"$date":"2020-13-45"}}"#, "$.v"),
        (r#"{"v":{"$date":20200101}}"#, "$.v"),
        (r#"{"v":[{"$datetime":"yesterday"}]}"#, "$.v[0]"),
        (r#"{"v":{"$duration":{"weeks":1}}}"#, "$.v"),
        (r#"{"v":{"$duration":{"days":1.5}}}"#, "$.v"),
        (r#"{"v":{"$duration":{"days":4294967296}}}"#, "$.v"),
        (r#"{"v":{"$duration":"P1D"}}"#, "$.v"),
    ] {
        let error = rejection(source);
        assert_eq!(
            error.kind(),
            JsonQueryParameterErrorKind::InvalidTemporal,
            "{source}"
        );
        assert_eq!(error.path(), path, "{source}");
    }
}

#[test]
fn tolerant_converter_decodes_the_same_tags_as_the_query_path() {
    let source = r#"{"d":{"$date":"2020-01-01"},"t":{"$datetime":"2020-01-01T10:00:00+02:00"},"s":[{"$duration":{"days":1}}]}"#;
    let parsed: serde_json::Value = serde_json::from_str(source).unwrap();
    let Value::Map(tolerant) = json_value_to_kglite_value(&parsed) else {
        panic!("object must convert to a map");
    };
    let strict = params(source);
    for key in ["d", "t", "s"] {
        assert_eq!(tolerant.get(key), strict.get(key), "{key}");
    }
    assert!(matches!(strict["d"], Value::DateTime(_)));
}

#[test]
fn tolerant_converter_keeps_an_invalid_tag_as_a_map() {
    let parsed: serde_json::Value = serde_json::from_str(r#"{"$date":"2020-13-45"}"#).unwrap();
    let Value::Map(map) = json_value_to_kglite_value(&parsed) else {
        panic!("an invalid tagged payload must stay a map");
    };
    assert_eq!(map.get("$date"), Some(&Value::String("2020-13-45".into())));
}

#[test]
fn float_tag_binds_every_non_finite_value_and_keeps_negative_zero() {
    let params =
        params(r#"{"n":{"$float":"NaN"},"p":{"$float":"inf"},"m":{"$float":"-inf"},"z":-0.0}"#);
    assert!(matches!(params["n"], Value::Float64(f) if f.is_nan()));
    assert_eq!(params["p"], Value::Float64(f64::INFINITY));
    assert_eq!(params["m"], Value::Float64(f64::NEG_INFINITY));
    assert!(matches!(params["z"], Value::Float64(f) if f == 0.0 && f.is_sign_negative()));
}

#[test]
fn invalid_float_tag_payload_is_refused_not_nulled() {
    for source in [
        r#"{"v":{"$float":"nan"}}"#,
        r#"{"v":{"$float":1.5}}"#,
        r#"{"v":[{"$float":null}]}"#,
    ] {
        let error = rejection(source);
        assert_eq!(
            error.kind(),
            JsonQueryParameterErrorKind::InvalidTemporal,
            "{source}"
        );
    }
}

#[test]
fn tagged_rendering_round_trips_non_finite_floats_everywhere() {
    let nested = Value::List(vec![
        Value::Float64(f64::NAN),
        Value::Float64(f64::INFINITY),
        Value::Float64(f64::NEG_INFINITY),
        Value::Float64(-0.0),
        Value::Float64(1.5),
    ]);
    let tagged = kglite_value_to_json_tagged(&nested);
    assert_eq!(
        tagged.to_string(),
        r#"[{"$float":"NaN"},{"$float":"inf"},{"$float":"-inf"},-0.0,1.5]"#
    );
    let Value::List(back) = json_value_to_kglite_value(&tagged) else {
        panic!("list expected");
    };
    assert!(matches!(back[0], Value::Float64(f) if f.is_nan()));
    assert_eq!(back[1], Value::Float64(f64::INFINITY));
    assert_eq!(back[2], Value::Float64(f64::NEG_INFINITY));
    assert!(matches!(back[3], Value::Float64(f) if f.is_sign_negative()));
    // The natural rendering keeps its legacy `null`.
    assert_eq!(
        kglite_value_to_json(&Value::Float64(f64::NAN)),
        serde_json::Value::Null
    );
    let point = kglite_value_to_json_tagged(&Value::Point {
        lat: f64::NAN,
        lon: 1.0,
    });
    assert_eq!(
        point,
        serde_json::json!({"$point": {"lat": {"$float": "NaN"}, "lon": 1.0}})
    );
}

fn typed_values() -> Vec<Value> {
    use crate::datatypes::PropMap;
    let date = chrono::NaiveDate::from_ymd_opt(2020, 1, 2).unwrap();
    let map = |k: &str, v: Value| Value::Map(PropMap::from_pairs(vec![(k.to_string(), v)]));
    vec![
        Value::DateTime(date),
        Value::Timestamp(date.and_hms_milli_opt(3, 4, 5, 250).unwrap()),
        Value::Duration {
            months: -1,
            days: 2,
            seconds: -3,
        },
        Value::Point {
            lat: 60.5,
            lon: -5.25,
        },
        Value::Float64(f64::INFINITY),
        Value::Float64(f64::NEG_INFINITY),
        // A map that spells a tag is escaped and comes back a map.
        map("$date", Value::String("2020-01-02".into())),
        map("$float", Value::String("NaN".into())),
        map("$map", Value::Int64(1)),
        map("$point", Value::Null),
        Value::List(vec![
            Value::DateTime(date),
            map("at", Value::DateTime(date)),
            Value::List(vec![Value::Float64(f64::INFINITY), Value::Null]),
        ]),
    ]
}

#[test]
fn tagged_rendering_round_trips_every_typed_value_as_a_parameter() {
    for value in typed_values() {
        let tagged = kglite_value_to_json_tagged(&value);
        // The tolerant converter.
        assert_eq!(json_value_to_kglite_value(&tagged), value, "{tagged}");
        // The checked query-parameter path, from text.
        let source = serde_json::json!({ "v": tagged }).to_string();
        let params = json_text_to_query_value_map(&source).unwrap();
        assert_eq!(params["v"], value, "{source}");
    }
}

#[test]
fn natural_rendering_does_not_round_trip_typed_values() {
    // Proves the round-trip test can fail: the untagged rendering loses the type.
    let date = Value::DateTime(chrono::NaiveDate::from_ymd_opt(2020, 1, 2).unwrap());
    let natural = kglite_value_to_json(&date);
    assert_eq!(natural, serde_json::json!("2020-01-02"));
    assert_ne!(json_value_to_kglite_value(&natural), date);
}

#[test]
fn tagged_non_finite_point_coordinate_round_trips() {
    let point = Value::Point {
        lat: f64::NAN,
        lon: f64::NEG_INFINITY,
    };
    let Value::Point { lat, lon } =
        json_value_to_kglite_value(&kglite_value_to_json_tagged(&point))
    else {
        panic!("point expected");
    };
    assert!(lat.is_nan());
    assert_eq!(lon, f64::NEG_INFINITY);
}

#[test]
fn tagged_node_properties_carry_their_types() {
    use crate::datatypes::values::NodeValue;
    use crate::datatypes::PropMap;
    let date = chrono::NaiveDate::from_ymd_opt(2020, 1, 2).unwrap();
    let node = Value::Node(Box::new(NodeValue {
        id: 7,
        labels: vec!["T".into()],
        properties: PropMap::from_pairs(vec![("d".to_string(), Value::DateTime(date))]),
    }));
    let tagged = kglite_value_to_json_tagged(&node);
    assert_eq!(
        tagged["properties"]["d"],
        serde_json::json!({"$date": "2020-01-02"})
    );
    assert_eq!(
        kglite_value_to_json(&node)["properties"]["d"],
        serde_json::json!("2020-01-02")
    );
}

#[test]
fn invalid_point_payloads_are_refused() {
    for source in [
        r#"{"v":{"$point":{"lat":1}}}"#,
        r#"{"v":{"$point":{"lat":1,"lon":2,"alt":3}}}"#,
        r#"{"v":{"$point":{"lat":"1","lon":2}}}"#,
        r#"{"v":{"$point":[1,2]}}"#,
    ] {
        let error = rejection(source);
        assert_eq!(
            error.kind(),
            JsonQueryParameterErrorKind::InvalidTemporal,
            "{source}"
        );
    }
}
