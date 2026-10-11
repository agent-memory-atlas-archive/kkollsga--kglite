//! `datetime()` and the zoned-datetime accessors. A `ZonedDateTime` is an
//! instant plus a fixed offset ([`crate::datatypes::zoned_datetime`]); a text or
//! map with no offset is read as UTC, and a named zone (`[Europe/Oslo]`) is
//! refused until zone rules ship.
use super::super::*;
use super::shared::parse_iso_datetime;
use super::temporal_iso::parse_date_text;
use super::temporal_map::{temporal_from_map, MapTarget};
use crate::datatypes::time_of_day::{format_offset, MAX_OFFSET_SECS};
use crate::datatypes::values::Value;
use crate::datatypes::zoned_datetime::{
    parse_with_offset, utc_from_wall, wall_clock, ZonedParseError,
};
use chrono::NaiveDateTime;

const NAMED_ZONE: &str =
    "named time zones are not supported yet; use a numeric offset such as +01:00";

/// The zoned value for a wall-clock reading at `offset_secs`.
pub(super) fn zoned_from_wall(wall: NaiveDateTime, offset_secs: i32) -> Result<Value, String> {
    utc_from_wall(wall, offset_secs)
        .map(|utc| Value::ZonedDateTime { utc, offset_secs })
        .ok_or_else(|| "datetime(): the instant is out of range".to_string())
}

/// Parse the text argument of `datetime(...)`. A text without an offset is UTC.
pub(super) fn parse_datetime_text(text: &str) -> Result<Value, String> {
    // The existing reader first (RFC 3339, minute-precision stamps, wide
    // years, the loose date forms); then the ISO spellings it does not know.
    if let Some(parsed) = parse_iso_datetime(text) {
        let offset_secs = parsed.offset.map_or(0, |o| o.local_minus_utc());
        if offset_secs.abs() > MAX_OFFSET_SECS {
            return Err(format!("datetime(): offset in '{text}' is beyond ±18:00"));
        }
        return zoned_from_wall(parsed.local, offset_secs);
    }
    match parse_with_offset(text, parse_date_text) {
        Ok((wall, offset)) => zoned_from_wall(wall, offset.unwrap_or(0)),
        Err(ZonedParseError::NamedZone) => Err(format!("datetime(): {NAMED_ZONE}")),
        Err(ZonedParseError::Malformed) => Err(format!(
            "datetime(): cannot parse '{text}' as an ISO-8601 datetime"
        )),
    }
}

impl CypherExecutor<'_> {
    /// `datetime()` (now, UTC), `datetime(string | map | temporal)`.
    pub(super) fn eval_datetime(
        &self,
        args: &[Expression],
        row: &ResultRow,
    ) -> Result<Value, String> {
        if args.is_empty() {
            return Ok(Value::ZonedDateTime {
                utc: chrono::Utc::now().naive_utc(),
                offset_secs: 0,
            });
        }
        if args.len() != 1 {
            return Err(
                "datetime() requires 0 or 1 argument: datetime() or datetime('2024-03-15T10:30:00+01:00')".into(),
            );
        }
        match self.evaluate_expression(&args[0], row)? {
            Value::String(s) => parse_datetime_text(&s),
            zoned @ Value::ZonedDateTime { .. } => Ok(zoned),
            // A local reading carries no offset: it names the same instant in UTC.
            Value::Timestamp(utc) => Ok(Value::ZonedDateTime {
                utc,
                offset_secs: 0,
            }),
            Value::DateTime(d) => Ok(Value::ZonedDateTime {
                utc: d.and_hms_opt(0, 0, 0).unwrap_or_default(),
                offset_secs: 0,
            }),
            Value::Map(map) => temporal_from_map(MapTarget::DateTime, &map),
            Value::Null => Ok(Value::Null),
            other => Err(format!(
                "datetime() argument must be a string, a map or a date or datetime, got {}",
                other.type_name()
            )),
        }
    }
}

/// The accessor `property` of a zoned datetime. The calendar and clock
/// components read the wall clock at the value's offset; `epochSeconds` and
/// `epochMillis` read the instant.
pub(super) fn zoned_property(
    utc: NaiveDateTime,
    offset_secs: i32,
    property: &str,
) -> Option<Value> {
    let int = |n: i64| Some(Value::Int64(n));
    match property {
        "offset" | "timezone" => Some(Value::String(format_offset(offset_secs))),
        "offsetSeconds" => int(i64::from(offset_secs)),
        "offsetMinutes" => int(i64::from(offset_secs) / 60),
        "epochSeconds" => int(utc.and_utc().timestamp()),
        "epochMillis" => int(utc.and_utc().timestamp_millis()),
        _ => super::temporal_accessors::datetime_part(wall_clock(utc, offset_secs), property)
            .map(Value::Int64),
    }
}

/// `localtime`, `time`, `localdatetime` readings of a zoned value share its
/// wall clock.
pub(super) fn wall_time_nanos(utc: NaiveDateTime, offset_secs: i32) -> u64 {
    let wall = wall_clock(utc, offset_secs);
    crate::datatypes::time_of_day::nanos_of(wall.time())
}
