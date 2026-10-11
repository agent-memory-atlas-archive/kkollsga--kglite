//! `datetime()` and the zoned-datetime accessors. A `ZonedDateTime` is an
//! instant plus an offset and, optionally, a named IANA zone
//! ([`crate::datatypes::zoned_datetime`]); a text or map with neither offset nor
//! zone is read as UTC.
use super::super::*;
use super::shared::parse_iso_datetime;
use super::temporal_iso::parse_date_text;
use super::temporal_map::{temporal_from_map, MapTarget};
use crate::datatypes::time_of_day::{format_offset, parse_offset, MAX_OFFSET_SECS};
use crate::datatypes::time_zone::ZoneId;
use crate::datatypes::values::Value;
use crate::datatypes::zoned_datetime::{
    parse_with_zone, resolve_wall, split_zone_suffix, utc_from_wall, wall_clock, zone_by_name,
    ZonedParts,
};
use chrono::NaiveDateTime;

/// How a constructor places a wall-clock reading in time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ZoneSpec {
    /// A fixed UTC offset.
    Fixed(i32),
    /// A named zone's rules. `preferred` is the offset a repeated reading keeps
    /// when it is one of the two (a base value's own offset).
    Named(ZoneId, Option<i32>),
}

impl ZoneSpec {
    /// The spec a `timezone: '...'` string names: a numeric offset or an IANA
    /// name.
    pub(super) fn from_text(function: &str, text: &str) -> Result<ZoneSpec, String> {
        if let Some(offset) = parse_offset(text) {
            return Ok(ZoneSpec::Fixed(offset));
        }
        if text.starts_with(['+', '-']) || text.eq_ignore_ascii_case("z") {
            return Err(format!(
                "{function}(): timezone '{text}' is not a valid offset"
            ));
        }
        zone_by_name(text)
            .map(|zone| ZoneSpec::Named(zone, None))
            .map_err(|e| format!("{function}(): {e}"))
    }

    /// The spec a zoned value already carries.
    pub(super) fn of_value(offset_secs: i32, zone: Option<ZoneId>) -> ZoneSpec {
        match zone {
            None => ZoneSpec::Fixed(offset_secs),
            Some(zone) => ZoneSpec::Named(zone, Some(offset_secs)),
        }
    }

    /// The fixed offset, `None` for a named zone.
    pub(super) fn fixed_offset(self) -> Option<i32> {
        match self {
            ZoneSpec::Fixed(offset) => Some(offset),
            ZoneSpec::Named(..) => None,
        }
    }
}

/// The zoned value for a wall-clock reading placed by `spec`. A reading a named
/// zone skipped moves forward by the gap; a repeated one takes the earlier
/// offset (or the preferred one).
pub(super) fn zoned_from_wall_in(wall: NaiveDateTime, spec: ZoneSpec) -> Result<Value, String> {
    let out_of_range = || "datetime(): the instant is out of range".to_string();
    match spec {
        ZoneSpec::Fixed(offset_secs) => zoned_from_wall(wall, offset_secs),
        ZoneSpec::Named(zone, preferred) => zone
            .resolve_local(wall, preferred)
            .map(|at| Value::ZonedDateTime {
                utc: at.utc,
                offset_secs: at.offset_secs,
                zone: Some(zone),
            })
            .ok_or_else(out_of_range),
    }
}

/// The zoned value for a wall-clock reading at `offset_secs`.
pub(super) fn zoned_from_wall(wall: NaiveDateTime, offset_secs: i32) -> Result<Value, String> {
    utc_from_wall(wall, offset_secs)
        .map(|utc| Value::zoned(utc, offset_secs))
        .ok_or_else(|| "datetime(): the instant is out of range".to_string())
}

/// Parse the text argument of `datetime(...)`. A text with neither offset nor
/// `[zone]` is UTC; a `[zone]` alone places the reading by the zone's rules;
/// both fix the instant by the offset and show it in the zone.
pub(super) fn parse_datetime_text(text: &str) -> Result<Value, String> {
    let malformed = || format!("datetime(): cannot parse '{text}' as an ISO-8601 datetime");
    let (body, zone_name) = split_zone_suffix(text.trim()).map_err(|_| malformed())?;
    let zone = zone_name
        .map(zone_by_name)
        .transpose()
        .map_err(|e| format!("datetime(): {e}"))?;
    // The existing reader first (RFC 3339, minute-precision stamps, wide
    // years, the loose date forms); then the ISO spellings it does not know.
    let (wall, offset) = if let Some(parsed) = parse_iso_datetime(body) {
        let offset_secs = parsed.offset.map(|o| o.local_minus_utc());
        if offset_secs.is_some_and(|o| o.abs() > MAX_OFFSET_SECS) {
            return Err(format!("datetime(): offset in '{text}' is beyond ±18:00"));
        }
        (parsed.local, offset_secs)
    } else {
        let parts = parse_with_zone(body, parse_date_text).map_err(|_| malformed())?;
        (parts.wall, parts.offset_secs)
    };
    resolve_wall(wall, offset, zone)
        .map(ZonedParts::into_value)
        .ok_or_else(|| "datetime(): the instant is out of range".to_string())
}

impl CypherExecutor<'_> {
    /// `datetime()` (the statement clock, UTC), `datetime(string | map | temporal)`.
    pub(super) fn eval_datetime(
        &self,
        args: &[Expression],
        row: &ResultRow,
    ) -> Result<Value, String> {
        if args.is_empty() {
            return Ok(Value::zoned(self.budget.statement_clock().utc(), 0));
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
            Value::Timestamp(utc) => Ok(Value::zoned(utc, 0)),
            Value::DateTime(d) => Ok(Value::zoned(d.and_hms_opt(0, 0, 0).unwrap_or_default(), 0)),
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
/// `epochMillis` read the instant; `timezone` is the IANA name when the value
/// has one, else its offset text.
pub(super) fn zoned_property(
    utc: NaiveDateTime,
    offset_secs: i32,
    zone: Option<ZoneId>,
    property: &str,
) -> Option<Value> {
    let int = |n: i64| Some(Value::Int64(n));
    match property {
        "timezone" => Some(Value::String(match zone {
            Some(zone) => zone.name().to_string(),
            None => format_offset(offset_secs),
        })),
        "offset" => Some(Value::String(format_offset(offset_secs))),
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
