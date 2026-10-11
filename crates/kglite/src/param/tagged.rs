//! Tagged JSON objects for query parameter types JSON has no spelling for.
//!
//! An object with exactly one key, `$date`, `$datetime`, `$zdatetime`,
//! `$localtime`, `$time`, `$duration`, `$point` or `$float`, is a typed value
//! rather than a map. The payloads are the shapes
//! [`super::kglite_value_to_json`] renders those types as, so a result cell
//! wrapped in its tag reads back as the value it came from:
//!
//! - `{"$date": "2020-01-01"}` → `Value::DateTime`, parsed as `date()` parses
//! - `{"$datetime": "2020-01-01T08:00:00"}` → `Value::Timestamp` (local); a
//!   text that carries an offset, `{"$datetime": "2020-01-01T08:00:00+02:00"}`,
//!   is a `Value::ZonedDateTime` keeping that offset
//! - `{"$zdatetime": "2020-01-01T08:00:00+02:00"}` → `Value::ZonedDateTime`; a
//!   text without an offset is read as UTC, as `datetime('...')` reads it
//! - `{"$localtime": "10:00:00.5"}` → `Value::LocalTime` (no offset allowed)
//! - `{"$time": "10:00:00+01:00"}` → `Value::Time`; a text without an offset
//!   is read as UTC, as `time('10:00')` is
//! - `{"$duration": {"months": 0, "days": 1, "seconds": 0}}` → `Value::Duration`
//!   (each field optional, default 0; no other field allowed)
//! - `{"$float": "NaN"}`, `{"$float": "inf"}`, `{"$float": "-inf"}` →
//!   `Value::Float64` of that non-finite value. JSON has no spelling for
//!   these; the tag is the one lossless spelling, for parameters and (opt-in)
//!   results alike. Any other payload is invalid.
//!
//! - `{"$point": {"lat": 60.1, "lon": 5.2}}` → `Value::Point`; each coordinate
//!   is a number or a `$float` tag, and no other field is allowed
//! - `{"$map": {…}}` → the plain map `{…}`: the escape for a map whose only key
//!   is itself a tag name, which would otherwise read as that tag
//!
//! Any other object, including one holding a tag key beside other keys, stays
//! an ordinary map.

use crate::datatypes::values::Value;

/// `None` when `map` is not a tagged object; `Some(None)` when it is one whose
/// payload is invalid.
pub(super) fn decode(map: &serde_json::Map<String, serde_json::Value>) -> Option<Option<Value>> {
    if map.len() != 1 {
        return None;
    }
    let (key, payload) = map.iter().next()?;
    Some(match key.as_str() {
        "$date" => payload.as_str().and_then(|text| {
            crate::graph::features::timeseries::parse_date_query(text)
                .ok()
                .map(|(date, _)| Value::DateTime(date))
        }),
        "$datetime" => payload.as_str().and_then(|text| {
            crate::graph::languages::cypher::executor::scalar_functions::parse_datetime_value(text)
        }),
        "$zdatetime" => payload.as_str().and_then(|text| {
            crate::datatypes::zoned_datetime::parse_zoned(text)
                .ok()
                .map(|(utc, offset_secs)| Value::ZonedDateTime { utc, offset_secs })
        }),
        "$localtime" => payload.as_str().and_then(|text| {
            match crate::datatypes::time_of_day::parse_time(text) {
                Ok((nanos, None)) => Some(Value::LocalTime(nanos)),
                _ => None,
            }
        }),
        "$time" => payload.as_str().and_then(|text| {
            crate::datatypes::time_of_day::parse_time(text)
                .ok()
                .map(|(nanos, offset)| Value::Time {
                    nanos,
                    offset_secs: offset.unwrap_or(0),
                })
        }),
        "$duration" => decode_duration(payload),
        "$point" => decode_point(payload),
        "$float" => match payload.as_str() {
            Some("NaN") => Some(Value::Float64(f64::NAN)),
            Some("inf") => Some(Value::Float64(f64::INFINITY)),
            Some("-inf") => Some(Value::Float64(f64::NEG_INFINITY)),
            _ => None,
        },
        _ => return None,
    })
}

/// The payload of a `{"$map": {…}}` escape, `None` for any other object.
pub(super) fn escaped_map(
    map: &serde_json::Map<String, serde_json::Value>,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    if map.len() != 1 {
        return None;
    }
    map.get("$map")?.as_object()
}

fn decode_point(payload: &serde_json::Value) -> Option<Value> {
    let fields = payload.as_object()?;
    if fields.len() != 2 {
        return None;
    }
    let coordinate = |name: &str| -> Option<f64> {
        let value = fields.get(name)?;
        match value {
            serde_json::Value::Number(n) => n.as_f64(),
            serde_json::Value::Object(tag) if tag.len() == 1 => match decode(tag)?? {
                Value::Float64(f) => Some(f),
                _ => None,
            },
            _ => None,
        }
    };
    Some(Value::Point {
        lat: coordinate("lat")?,
        lon: coordinate("lon")?,
    })
}

pub(super) fn decode_duration(payload: &serde_json::Value) -> Option<Value> {
    let fields = payload.as_object()?;
    let (mut months, mut days, mut seconds, mut nanos) = (0i32, 0i32, 0i64, 0i32);
    for (name, value) in fields {
        let value = value.as_i64()?;
        match name.as_str() {
            "months" => months = i32::try_from(value).ok()?,
            "days" => days = i32::try_from(value).ok()?,
            "seconds" => seconds = value,
            // Sub-second part, always non-negative (the sign lives in seconds).
            "nanoseconds" => {
                nanos = i32::try_from(value)
                    .ok()
                    .filter(|n| (0..1_000_000_000).contains(n))?
            }
            _ => return None,
        }
    }
    Some(Value::Duration {
        months,
        days,
        seconds,
        nanos,
    })
}

#[cfg(test)]
#[path = "tagged_tests.rs"]
mod tests;
