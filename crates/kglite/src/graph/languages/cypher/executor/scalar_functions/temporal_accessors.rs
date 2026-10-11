//! `.year`, `.week`, `.hour`, `.quarters` and the other component accessors of
//! a date, a datetime or a duration. One table behind both property-access
//! paths (`d.year` on a projected variable and `f(x).year` on an expression).
use crate::datatypes::values::Value;
use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};

fn date_part(date: NaiveDate, property: &str) -> Option<i64> {
    Some(match property {
        "year" => date.year() as i64,
        "quarter" => (date.month0() / 3 + 1) as i64,
        "month" => date.month() as i64,
        "week" => date.iso_week().week() as i64,
        "weekYear" => date.iso_week().year() as i64,
        "day" => date.day() as i64,
        "ordinalDay" | "dayOfYear" => date.ordinal() as i64,
        "weekDay" | "dayOfWeek" => date.weekday().number_from_monday() as i64,
        "dayOfQuarter" => {
            let first = NaiveDate::from_ymd_opt(date.year(), date.month0() / 3 * 3 + 1, 1)?;
            (date - first).num_days() + 1
        }
        _ => return None,
    })
}

fn datetime_part(t: NaiveDateTime, property: &str) -> Option<i64> {
    Some(match property {
        "hour" => t.hour() as i64,
        "minute" => t.minute() as i64,
        "second" => t.second() as i64,
        "millisecond" => (t.nanosecond() / 1_000_000) as i64,
        "microsecond" => (t.nanosecond() / 1_000) as i64,
        "nanosecond" => t.nanosecond() as i64,
        "epochSeconds" => t.and_utc().timestamp(),
        "epochMillis" => t.and_utc().timestamp_millis(),
        _ => return date_part(t.date(), property),
    })
}

/// `nanos` is the non-negative sub-second remainder; the unit totals
/// (`milliseconds`..) floor over `seconds * 1e9 + nanos`, so `-0.5 s`
/// (`seconds = -1`) reads `-500` milliseconds.
fn duration_part(months: i32, days: i32, seconds: i64, nanos: i32, property: &str) -> Option<i64> {
    let total = crate::graph::core::value_operations::clock_total(seconds, nanos);
    let (months, days) = (months as i64, days as i64);
    Some(match property {
        "years" => months / 12,
        "quarters" => months / 3,
        "months" => months,
        "weeks" => days / 7,
        "days" => days,
        "hours" => seconds / 3600,
        "minutes" => seconds / 60,
        "seconds" => seconds,
        "milliseconds" => i64::try_from(total.div_euclid(1_000_000)).ok()?,
        "microseconds" => i64::try_from(total.div_euclid(1_000)).ok()?,
        "nanoseconds" => i64::try_from(total).ok()?,
        "quartersOfYear" => (months % 12) / 3,
        "monthsOfQuarter" => months % 3,
        "monthsOfYear" => months % 12,
        "daysOfWeek" => days % 7,
        "minutesOfHour" => (seconds / 60) % 60,
        "secondsOfMinute" => seconds % 60,
        "millisecondsOfSecond" => i64::from(nanos / 1_000_000),
        "microsecondsOfSecond" => i64::from(nanos / 1_000),
        "nanosecondsOfSecond" => i64::from(nanos),
        _ => return None,
    })
}

/// The accessor `property` of a temporal value: `Some(component)` or
/// `Some(Null)` for a temporal value that has no such component, and `None`
/// when `value` is not a date, datetime or duration at all.
pub(in crate::graph::languages::cypher::executor) fn temporal_property(
    value: &Value,
    property: &str,
) -> Option<Value> {
    let part = match value {
        Value::DateTime(date) => match property {
            // A date has a midnight time of day.
            "hour" | "minute" | "second" => Some(0),
            "epochSeconds" => date.and_hms_opt(0, 0, 0).map(|t| t.and_utc().timestamp()),
            "epochMillis" => date
                .and_hms_opt(0, 0, 0)
                .map(|t| t.and_utc().timestamp_millis()),
            _ => date_part(*date, property),
        },
        Value::Timestamp(t) => datetime_part(*t, property),
        Value::LocalTime(nanos) => {
            return Some(super::temporal_time::time_property(*nanos, None, property))
        }
        Value::Time { nanos, offset_secs } => {
            return Some(super::temporal_time::time_property(
                *nanos,
                Some(*offset_secs),
                property,
            ))
        }
        Value::Duration {
            months,
            days,
            seconds,
            nanos,
        } => duration_part(*months, *days, *seconds, *nanos, property),
        _ => return None,
    };
    Some(part.map_or(Value::Null, Value::Int64))
}
