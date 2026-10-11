//! The JSON a list or map cell holds in the CSV and RDF exports.
//!
//! JSON has no spelling for a date, timestamp, duration or point, nor for a
//! non-finite float, so a value of one of those kinds *inside* a list or map is
//! written as a one-key tagged object and read back as the value it came from.
//! Everything JSON can say is written plain, so an export without such values is
//! unchanged and any plain JSON array or object reads back as a list or map.
//!
//! | value | JSON |
//! |-------|------|
//! | date | `{"$date": "2020-01-01"}` |
//! | timestamp | `{"$datetime": "2020-01-02T03:04:05.250"}` (no zone) |
//! | duration | `{"$duration": {"months": 0, "days": 1, "seconds": 0}}` |
//! | point | `{"$point": {"lat": 60.1, "lon": 5.2}}` |
//! | NaN, infinities | `{"$float": "NaN"}`, `"inf"`, `"-inf"` |
//! | a map whose only key is one of these tags | `{"$map": { ... }}` |
//!
//! The date, datetime and duration spellings are those of the query-parameter
//! convention. A tagged object with an invalid payload reads as an ordinary map.

use chrono::{Datelike, NaiveDate, NaiveDateTime};
use serde_json::{json, Map, Value as Json};

use crate::datatypes::prop_map::PropMap;
use crate::datatypes::time_of_day::{format_clock, format_time, parse_time};
use crate::datatypes::values::{raw_string, Value};
use crate::datatypes::zoned_datetime::{format_zoned_in, parse_zoned};

const TAGS: [&str; 9] = [
    "$date",
    "$datetime",
    "$localtime",
    "$time",
    "$zdatetime",
    "$duration",
    "$point",
    "$float",
    "$map",
];
const TIMESTAMP_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.f";

fn tagged(tag: &str, payload: Json) -> Json {
    let mut object = Map::new();
    object.insert(tag.to_string(), payload);
    Json::Object(object)
}

/// A value as JSON, typed values tagged.
pub(crate) fn to_json(value: &Value) -> Json {
    match value {
        Value::Null | Value::NodeRef(_) => Json::Null,
        Value::String(s) => json!(s),
        Value::Int64(n) => json!(n),
        Value::UniqueId(n) => json!(n),
        Value::Float64(f) => match serde_json::Number::from_f64(*f) {
            Some(number) => Json::Number(number),
            None => tagged(
                "$float",
                json!(if f.is_nan() {
                    "NaN"
                } else if *f > 0.0 {
                    "inf"
                } else {
                    "-inf"
                }),
            ),
        },
        Value::Boolean(b) => json!(b),
        Value::DateTime(d) => tagged("$date", json!(d.to_string())),
        Value::Timestamp(t) => tagged("$datetime", json!(t.format(TIMESTAMP_FORMAT).to_string())),
        Value::LocalTime(n) => tagged("$localtime", json!(format_clock(*n))),
        Value::Time { nanos, offset_secs } => {
            tagged("$time", json!(format_time(*nanos, *offset_secs)))
        }
        Value::ZonedDateTime {
            utc,
            offset_secs,
            zone,
        } => tagged(
            "$zdatetime",
            json!(format_zoned_in(*utc, *offset_secs, *zone)),
        ),
        Value::Point { lat, lon } => tagged("$point", json!({"lat": lat, "lon": lon})),
        Value::Duration {
            months,
            days,
            seconds,
            nanos,
        } => tagged(
            "$duration",
            duration_fields(*months, *days, *seconds, *nanos),
        ),
        Value::List(items) => Json::Array(items.iter().map(to_json).collect()),
        Value::Map(map) => {
            let object: Map<String, Json> = map
                .iter()
                .map(|(k, v)| (k.to_string(), to_json(v)))
                .collect();
            if object.len() == 1 && TAGS.contains(&object.keys().next().unwrap().as_str()) {
                tagged("$map", Json::Object(object))
            } else {
                Json::Object(object)
            }
        }
        other => json!(raw_string(other)),
    }
}

/// The `$duration` payload; `nanoseconds` is omitted at 0 so whole-second
/// durations keep their original three-key shape.
pub(crate) fn duration_fields(months: i32, days: i32, seconds: i64, nanos: i32) -> Json {
    let mut fields = json!({"months": months, "days": days, "seconds": seconds});
    if nanos != 0 {
        fields["nanoseconds"] = json!(nanos);
    }
    fields
}

fn decode_tagged(object: &Map<String, Json>) -> Option<Value> {
    if object.len() != 1 {
        return None;
    }
    let (tag, payload) = object.iter().next()?;
    match tag.as_str() {
        "$date" => NaiveDate::parse_from_str(payload.as_str()?, "%Y-%m-%d")
            .ok()
            .filter(|d| (1..=9999).contains(&d.year()))
            .map(Value::DateTime),
        "$datetime" => NaiveDateTime::parse_from_str(payload.as_str()?, TIMESTAMP_FORMAT)
            .ok()
            .filter(|t| (1..=9999).contains(&t.year()))
            .map(Value::Timestamp),
        "$localtime" => match parse_time(payload.as_str()?) {
            Ok((nanos, None)) => Some(Value::LocalTime(nanos)),
            _ => None,
        },
        "$time" => match parse_time(payload.as_str()?) {
            Ok((nanos, Some(offset_secs))) => Some(Value::Time { nanos, offset_secs }),
            _ => None,
        },
        "$zdatetime" => {
            let parts = parse_zoned(payload.as_str()?).ok()?;
            (1..=9999)
                .contains(&parts.utc.year())
                .then(|| parts.into_value())
        }
        "$duration" => {
            let fields = payload.as_object()?;
            let field = |name: &str| fields.get(name).and_then(Json::as_i64);
            Some(Value::Duration {
                months: i32::try_from(field("months")?).ok()?,
                days: i32::try_from(field("days")?).ok()?,
                seconds: field("seconds")?,
                nanos: match fields.get("nanoseconds") {
                    None => 0,
                    Some(n) => i32::try_from(n.as_i64()?)
                        .ok()
                        .filter(|n| (0..1_000_000_000).contains(n))?,
                },
            })
        }
        "$point" => {
            let fields = payload.as_object()?;
            Some(Value::Point {
                lat: fields.get("lat")?.as_f64()?,
                lon: fields.get("lon")?.as_f64()?,
            })
        }
        "$float" => match payload.as_str()? {
            "NaN" => Some(Value::Float64(f64::NAN)),
            "inf" => Some(Value::Float64(f64::INFINITY)),
            "-inf" => Some(Value::Float64(f64::NEG_INFINITY)),
            _ => None,
        },
        "$map" => payload.as_object().map(plain_map),
        _ => None,
    }
}

fn plain_map(object: &Map<String, Json>) -> Value {
    Value::Map(PropMap::from_pairs(
        object
            .iter()
            .map(|(k, v)| (k.clone(), from_json(v)))
            .collect(),
    ))
}

/// A JSON value as a graph value: arrays become lists, objects maps, tagged
/// objects the typed value they stand for.
pub(crate) fn from_json(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => n
            .as_i64()
            .map(Value::Int64)
            .or_else(|| n.as_f64().map(Value::Float64))
            .unwrap_or(Value::Null),
        Json::String(s) => Value::String(s.clone()),
        Json::Array(items) => Value::List(items.iter().map(from_json).collect()),
        Json::Object(object) => decode_tagged(object).unwrap_or_else(|| plain_map(object)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(value: &Value) -> Value {
        let text = to_json(value).to_string();
        from_json(&serde_json::from_str(&text).unwrap())
    }

    #[test]
    fn typed_values_inside_containers_survive() {
        let date = Value::DateTime(NaiveDate::from_ymd_opt(2020, 1, 2).unwrap());
        let ts = Value::Timestamp(
            NaiveDate::from_ymd_opt(2020, 1, 2)
                .unwrap()
                .and_hms_milli_opt(3, 4, 5, 250)
                .unwrap(),
        );
        let duration = Value::Duration {
            months: -1,
            days: 2,
            seconds: -3,
            nanos: 0,
        };
        let point = Value::Point {
            lat: 60.5,
            lon: -5.25,
        };
        let list = Value::List(vec![
            date,
            ts,
            duration,
            point,
            Value::Float64(f64::INFINITY),
            Value::Int64(7),
            Value::String("$date".into()),
            Value::List(vec![Value::Null, Value::Boolean(true)]),
        ]);
        assert_eq!(round(&list), list);
    }

    #[test]
    fn times_of_day_survive_with_their_offset() {
        let values = [
            Value::LocalTime(36_000_500_000_000),
            Value::Time {
                nanos: 86_399_999_999_999,
                offset_secs: -19_800,
            },
            Value::Time {
                nanos: 0,
                offset_secs: 0,
            },
        ];
        for value in values {
            // `==` ignores a Time's offset; the debug form carries it.
            assert_eq!(format!("{:?}", round(&value)), format!("{value:?}"));
            let nested = Value::List(vec![value.clone()]);
            assert_eq!(format!("{:?}", round(&nested)), format!("{nested:?}"));
        }
        assert_eq!(
            to_json(&Value::Time {
                nanos: 36_000_000_000_000,
                offset_secs: 3600
            })
            .to_string(),
            r#"{"$time":"10:00+01:00"}"#
        );
        // A `$localtime` with an offset, and a `$time` without one, are not typed values.
        for bad in [r#"{"$localtime":"10:00Z"}"#, r#"{"$time":"10:00"}"#] {
            let json: Json = serde_json::from_str(bad).unwrap();
            assert!(matches!(from_json(&json), Value::Map(_)), "{bad}");
        }
    }

    #[test]
    fn a_map_that_looks_tagged_is_escaped() {
        let map = |k: &str, v: Value| Value::Map(PropMap::from_pairs(vec![(k.to_string(), v)]));
        for tag in TAGS {
            let value = map(tag, Value::String("2020-01-01".into()));
            assert_eq!(round(&value), value, "{tag}");
        }
        let nested = map("$map", map("$map", Value::Int64(1)));
        assert_eq!(round(&nested), nested);
    }

    #[test]
    fn plain_json_stays_plain_and_a_bad_payload_is_a_map() {
        assert_eq!(
            to_json(&Value::List(vec![
                Value::Int64(1),
                Value::String("a".into())
            ]))
            .to_string(),
            "[1,\"a\"]"
        );
        let bad: Json = serde_json::from_str(r#"{"$date": "nope"}"#).unwrap();
        assert!(matches!(from_json(&bad), Value::Map(_)));
    }

    #[test]
    fn dates_outside_the_supported_years_are_not_dates() {
        let far: Json = serde_json::from_str(r#"{"$date": "+10000-01-01"}"#).unwrap();
        assert!(matches!(from_json(&far), Value::Map(_)));
    }
}
