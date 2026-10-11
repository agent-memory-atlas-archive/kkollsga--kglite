//! `date.truncate`, `localdatetime.truncate`, `datetime.truncate`,
//! `localtime.truncate` and `time.truncate`: round a temporal value down to
//! the start of a calendar or clock unit, then apply the optional override map
//! with the constructor's map semantics (`date.truncate('month', d, {day: 5})`).
use super::super::*;
use super::temporal_iso::iso_week_one_monday;
use super::temporal_map::{base_datetime, temporal_from_map, MapTarget};
use crate::datatypes::values::Value;
use crate::datatypes::PropMap;
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

/// Units coarser than a day only a date-bearing target can truncate to.
const CALENDAR_UNITS: &[&str] = &[
    "millennium",
    "century",
    "decade",
    "year",
    "weekyear",
    "quarter",
    "month",
    "week",
];
const CLOCK_UNITS: &[&str] = &[
    "day",
    "hour",
    "minute",
    "second",
    "millisecond",
    "microsecond",
];

fn truncate_to(unit: &str, value: NaiveDateTime) -> Result<NaiveDateTime, String> {
    let date = value.date();
    let midnight = |d: Option<NaiveDate>| {
        d.and_then(|d| d.and_hms_opt(0, 0, 0))
            .ok_or_else(|| "truncate(): result out of range".to_string())
    };
    let time = value.time();
    let clock = |h: u32, m: u32, s: u32, n: u32| {
        NaiveTime::from_hms_nano_opt(h, m, s, n).map(|t| date.and_time(t))
    };
    let out = match unit {
        "millennium" => midnight(NaiveDate::from_ymd_opt(
            date.year().div_euclid(1000) * 1000,
            1,
            1,
        ))?,
        "century" => midnight(NaiveDate::from_ymd_opt(
            date.year().div_euclid(100) * 100,
            1,
            1,
        ))?,
        "decade" => midnight(NaiveDate::from_ymd_opt(
            date.year().div_euclid(10) * 10,
            1,
            1,
        ))?,
        "year" => midnight(NaiveDate::from_ymd_opt(date.year(), 1, 1))?,
        "weekyear" => midnight(iso_week_one_monday(date.iso_week().year()))?,
        "quarter" => midnight(NaiveDate::from_ymd_opt(
            date.year(),
            date.month0() / 3 * 3 + 1,
            1,
        ))?,
        "month" => midnight(NaiveDate::from_ymd_opt(date.year(), date.month(), 1))?,
        "week" => midnight(date.checked_sub_signed(chrono::TimeDelta::days(
            date.weekday().num_days_from_monday() as i64,
        )))?,
        "day" => midnight(Some(date))?,
        "hour" => clock(time.hour(), 0, 0, 0).ok_or("truncate(): bad time")?,
        "minute" => clock(time.hour(), time.minute(), 0, 0).ok_or("truncate(): bad time")?,
        "second" => {
            clock(time.hour(), time.minute(), time.second(), 0).ok_or("truncate(): bad time")?
        }
        "millisecond" => clock(
            time.hour(),
            time.minute(),
            time.second(),
            time.nanosecond() / 1_000_000 * 1_000_000,
        )
        .ok_or("truncate(): bad time")?,
        "microsecond" => clock(
            time.hour(),
            time.minute(),
            time.second(),
            time.nanosecond() / 1_000 * 1_000,
        )
        .ok_or("truncate(): bad time")?,
        other => return Err(format!("truncate(): unknown unit '{other}'")),
    };
    Ok(out)
}

/// Whether `name` is one of the `<type>.truncate` or `duration.in*` functions.
pub(super) fn is_namespaced(name: &str) -> bool {
    name.ends_with(".truncate")
        || matches!(
            name,
            "duration.inmonths" | "duration.indays" | "duration.inseconds"
        )
}

impl CypherExecutor<'_> {
    /// The `<type>.<verb>` temporal functions: `.truncate` and the
    /// `duration.in*` counters.
    pub(super) fn eval_namespaced_temporal(
        &self,
        name: &str,
        args: &[Expression],
        row: &ResultRow,
    ) -> Result<Value, String> {
        if name.starts_with("duration.") {
            self.eval_duration_in(name, args, row)
        } else {
            self.eval_truncate(name, args, row)
        }
    }

    /// `<type>.truncate(unit, temporal [, overrides])`; `name` is the
    /// lowercased dispatcher name.
    pub(super) fn eval_truncate(
        &self,
        name: &str,
        args: &[Expression],
        row: &ResultRow,
    ) -> Result<Value, String> {
        if args.len() != 2 && args.len() != 3 {
            return Err(format!(
                "{name}() requires 2 or 3 arguments: {name}(unit, temporal [, overrides])"
            ));
        }
        let target = match name {
            "date.truncate" => MapTarget::Date,
            "localtime.truncate" => MapTarget::LocalTime,
            "time.truncate" => MapTarget::Time,
            "localdatetime.truncate" => MapTarget::LocalDateTime,
            _ => MapTarget::DateTime,
        };
        let unit = self.evaluate_expression(&args[0], row)?;
        let temporal = self.evaluate_expression(&args[1], row)?;
        let overrides = match args.get(2) {
            Some(expr) => self.evaluate_expression(expr, row)?,
            None => Value::Map(PropMap::new()),
        };
        if matches!(unit, Value::Null) || matches!(temporal, Value::Null) {
            return Ok(Value::Null);
        }
        let Value::String(unit) = unit else {
            return Err(format!("{name}(): the unit must be a string"));
        };
        let unit = unit.to_ascii_lowercase();
        let allowed = match target {
            MapTarget::Date => CALENDAR_UNITS.iter().chain(&["day"]).any(|u| *u == unit),
            MapTarget::LocalTime | MapTarget::Time => CLOCK_UNITS.contains(&unit.as_str()),
            _ => CALENDAR_UNITS.contains(&unit.as_str()) || CLOCK_UNITS.contains(&unit.as_str()),
        };
        if !allowed {
            return Err(format!(
                "{name}(): unit '{unit}' is not valid for this type"
            ));
        }
        let Some(base) = base_datetime(&temporal) else {
            return Err(format!(
                "{name}(): the second argument must be a temporal value, got {}",
                temporal.type_name()
            ));
        };
        let truncated = truncate_to(&unit, base)?;
        let mut map = match overrides {
            Value::Map(map) => map,
            Value::Null => PropMap::new(),
            other => {
                return Err(format!(
                    "{name}(): the override argument must be a map, got {}",
                    other.type_name()
                ))
            }
        };
        match target {
            MapTarget::Date => map.insert("date", Value::DateTime(truncated.date())),
            MapTarget::LocalTime => map.insert(
                "time",
                Value::LocalTime(crate::datatypes::time_of_day::nanos_of(truncated.time())),
            ),
            // The truncated clock keeps the offset it had (UTC for a local value);
            // a `timezone` override re-places it, so it is handed over as local.
            MapTarget::Time if map.contains_key("timezone") => map.insert(
                "time",
                Value::LocalTime(crate::datatypes::time_of_day::nanos_of(truncated.time())),
            ),
            MapTarget::Time => map.insert(
                "time",
                Value::Time {
                    nanos: crate::datatypes::time_of_day::nanos_of(truncated.time()),
                    offset_secs: match &temporal {
                        Value::Time { offset_secs, .. } => *offset_secs,
                        _ => 0,
                    },
                },
            ),
            // A zoned datetime truncates its wall clock and keeps its offset and
            // zone (UTC for a local value). A `timezone` override re-places the
            // truncated reading, so it is handed over as a local one.
            MapTarget::DateTime if map.contains_key("timezone") => {
                map.insert("datetime", Value::Timestamp(truncated))
            }
            MapTarget::DateTime => {
                let spec = match &temporal {
                    Value::ZonedDateTime {
                        offset_secs, zone, ..
                    } => super::temporal_zoned::ZoneSpec::of_value(*offset_secs, *zone),
                    _ => super::temporal_zoned::ZoneSpec::Fixed(0),
                };
                map.insert(
                    "datetime",
                    super::temporal_zoned::zoned_from_wall_in(truncated, spec)?,
                )
            }
            _ => map.insert("datetime", Value::Timestamp(truncated)),
        };
        temporal_from_map(target, &map)
    }
}
