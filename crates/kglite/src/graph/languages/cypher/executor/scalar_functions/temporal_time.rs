//! `localtime()` and `time()`: the constructors of the two time-of-day values,
//! their accessors, and `duration.between` over them. Parsing and the
//! canonical text form live in [`crate::datatypes::time_of_day`].
use super::super::*;
use super::temporal_map::{temporal_from_map, MapTarget};
use crate::datatypes::time_of_day::{
    format_offset, nanos_of, parse_time, utc_nanos, TimeParseError,
};
use crate::datatypes::values::Value;

/// Parse the text argument of `localtime(...)` / `time(...)`.
fn parse_text(function: &str, text: &str) -> Result<(u64, Option<i32>), String> {
    parse_time(text).map_err(|e| match e {
        TimeParseError::NamedZone => format!(
            "{function}(): named time zones are not supported on time values; use a numeric offset such as +01:00"
        ),
        TimeParseError::Malformed => format!(
            "{function}(): cannot parse '{text}' as a time (HH, HH:MM, HH:MM:SS or HH:MM:SS.fff, with an optional Z or +HH:MM offset)"
        ),
    })
}

impl CypherExecutor<'_> {
    /// `localtime()` (the local wall clock), `localtime(string | map | temporal)`.
    /// An offset on the text or on a `Time` is dropped: the wall-clock reading
    /// is kept, as `localdatetime()` keeps it.
    pub(super) fn eval_localtime(
        &self,
        args: &[Expression],
        row: &ResultRow,
    ) -> Result<Value, String> {
        if args.is_empty() {
            return Ok(Value::LocalTime(nanos_of(chrono::Local::now().time())));
        }
        if args.len() != 1 {
            return Err(
                "localtime() takes 0 or 1 argument: localtime() or localtime('10:00')".into(),
            );
        }
        match self.evaluate_expression(&args[0], row)? {
            Value::String(s) => {
                parse_text("localtime", &s).map(|(nanos, _)| Value::LocalTime(nanos))
            }
            Value::Map(map) => temporal_from_map(MapTarget::LocalTime, &map),
            local @ Value::LocalTime(_) => Ok(local),
            Value::Time { nanos, .. } => Ok(Value::LocalTime(nanos)),
            Value::Timestamp(t) => Ok(Value::LocalTime(nanos_of(t.time()))),
            Value::Null => Ok(Value::Null),
            other => Err(format!(
                "localtime() argument must be a string, a map or a time or datetime, got {}",
                other.type_name()
            )),
        }
    }

    /// `time()` (now, in UTC), `time(string | map | temporal)`. A reading with
    /// no offset is UTC, as `datetime()` reads one.
    pub(super) fn eval_time(&self, args: &[Expression], row: &ResultRow) -> Result<Value, String> {
        if args.is_empty() {
            return Ok(Value::Time {
                nanos: nanos_of(chrono::Utc::now().time()),
                offset_secs: 0,
            });
        }
        if args.len() != 1 {
            return Err("time() takes 0 or 1 argument: time() or time('10:00+01:00')".into());
        }
        match self.evaluate_expression(&args[0], row)? {
            Value::String(s) => parse_text("time", &s).map(|(nanos, offset)| Value::Time {
                nanos,
                offset_secs: offset.unwrap_or(0),
            }),
            Value::Map(map) => temporal_from_map(MapTarget::Time, &map),
            time @ Value::Time { .. } => Ok(time),
            Value::LocalTime(nanos) => Ok(Value::Time {
                nanos,
                offset_secs: 0,
            }),
            Value::Timestamp(t) => Ok(Value::Time {
                nanos: nanos_of(t.time()),
                offset_secs: 0,
            }),
            Value::Null => Ok(Value::Null),
            other => Err(format!(
                "time() argument must be a string, a map or a time or datetime, got {}",
                other.type_name()
            )),
        }
    }
}

/// The accessor `property` of a time of day. `offset` is `None` for a
/// `LocalTime`, which has none, so the offset accessors answer null.
pub(super) fn time_property(nanos: u64, offset: Option<i32>, property: &str) -> Value {
    let sub = nanos % 1_000_000_000;
    let int = |n: u64| Value::Int64(n as i64);
    match (property, offset) {
        ("hour", _) => int(nanos / 3_600_000_000_000),
        ("minute", _) => int(nanos / 60_000_000_000 % 60),
        ("second", _) => int(nanos / 1_000_000_000 % 60),
        ("millisecond", _) => int(sub / 1_000_000),
        ("microsecond", _) => int(sub / 1_000),
        ("nanosecond", _) => int(sub),
        ("offset" | "timezone", Some(off)) => Value::String(format_offset(off)),
        ("offsetSeconds", Some(off)) => Value::Int64(i64::from(off)),
        ("offsetMinutes", Some(off)) => Value::Int64(i64::from(off) / 60),
        _ => Value::Null,
    }
}

/// `duration.between` when either side is a time of day: `None` if neither is.
/// The result carries seconds and nanoseconds only. Two zoned times compare at
/// their UTC readings; a local time against a zoned one compares clock readings.
pub(super) fn time_between(a: &Value, b: &Value) -> Option<Result<Value, String>> {
    let is_time = |v: &Value| matches!(v, Value::LocalTime(_) | Value::Time { .. });
    if !is_time(a) && !is_time(b) {
        return None;
    }
    let both_zoned = matches!((a, b), (Value::Time { .. }, Value::Time { .. }));
    let reading = |v: &Value| match v {
        Value::LocalTime(n) => Some(i128::from(*n)),
        Value::Time { nanos, offset_secs } if both_zoned => {
            Some(i128::from(utc_nanos(*nanos, *offset_secs)))
        }
        Value::Time { nanos, .. } => Some(i128::from(*nanos)),
        _ => None,
    };
    Some(match (reading(a), reading(b)) {
        (Some(x), Some(y)) => Ok(clock_delta(y - x)),
        _ => Err("duration.between() cannot mix a time of day with a date or datetime".into()),
    })
}

fn clock_delta(total_nanos: i128) -> Value {
    // |delta| is below 2 days of nanoseconds: always representable.
    let (seconds, nanos) = crate::graph::core::value_operations::split_clock(total_nanos)
        .expect("a time-of-day delta fits a duration");
    Value::Duration {
        months: 0,
        days: 0,
        seconds,
        nanos,
    }
}
