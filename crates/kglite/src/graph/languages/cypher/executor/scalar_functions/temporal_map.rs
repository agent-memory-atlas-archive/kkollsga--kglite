//! The map forms of `date()`, `localtime()`, `localdatetime()` and
//! `datetime()`: calendar components in any of four spellings (year-month-day,
//! ISO week, ordinal day, quarter), clock components, and an existing temporal
//! value to copy the unnamed components from (`{date: d, hour: 10}`).
use super::temporal_iso::{
    date_from_iso_week, date_from_ordinal, date_from_quarter, day_of_quarter, parse_date_text,
    parse_local_datetime_text, parse_time_text,
};
use crate::datatypes::time_of_day::{naive_time, nanos_of, parse_offset, parse_time};
use crate::datatypes::values::Value;
use crate::datatypes::PropMap;
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

/// What a map is being built into.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MapTarget {
    Date,
    LocalTime,
    /// A time of day with an offset (`timezone`, or the `time` base's, or UTC).
    Time,
    LocalDateTime,
    /// Zoneless like `LocalDateTime`: this engine stores datetimes in UTC.
    DateTime,
}

const DATE_KEYS: &[&str] = &[
    "year",
    "month",
    "day",
    "week",
    "dayOfWeek",
    "ordinalDay",
    "quarter",
    "dayOfQuarter",
];
const TIME_KEYS: &[&str] = &[
    "hour",
    "minute",
    "second",
    "millisecond",
    "microsecond",
    "nanosecond",
];

impl MapTarget {
    fn name(self) -> &'static str {
        match self {
            MapTarget::Date => "date",
            MapTarget::LocalTime => "localtime",
            MapTarget::Time => "time",
            MapTarget::LocalDateTime => "localdatetime",
            MapTarget::DateTime => "datetime",
        }
    }

    fn accepts(self, key: &str) -> bool {
        let date = DATE_KEYS.contains(&key) || key == "date";
        let time = TIME_KEYS.contains(&key) || key == "time";
        match self {
            MapTarget::Date => date,
            MapTarget::LocalTime => time,
            MapTarget::Time => time || key == "timezone",
            MapTarget::LocalDateTime | MapTarget::DateTime => date || time || key == "datetime",
        }
    }

    /// Whether the target carries a calendar date.
    fn has_date(self) -> bool {
        !matches!(self, MapTarget::LocalTime | MapTarget::Time)
    }

    fn accepted(self) -> &'static str {
        match self {
            MapTarget::Date => {
                "year, month, day, week, dayOfWeek, ordinalDay, quarter, dayOfQuarter, date"
            }
            MapTarget::LocalTime => {
                "hour, minute, second, millisecond, microsecond, nanosecond, time"
            }
            MapTarget::Time => {
                "hour, minute, second, millisecond, microsecond, nanosecond, time, timezone"
            }
            MapTarget::LocalDateTime | MapTarget::DateTime => {
                "year, month, day, week, dayOfWeek, ordinalDay, quarter, dayOfQuarter, hour, minute, second, millisecond, microsecond, nanosecond, date, time, datetime"
            }
        }
    }
}

/// A temporal value (or its text) read as the datetime it names: a date is
/// midnight, a clock-only string is the epoch day.
pub(super) fn base_datetime(value: &Value) -> Option<NaiveDateTime> {
    match value {
        Value::DateTime(d) => d.and_hms_opt(0, 0, 0),
        Value::Timestamp(t) => Some(*t),
        Value::LocalTime(n) | Value::Time { nanos: n, .. } => {
            naive_time(*n).map(|t| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap().and_time(t))
        }
        Value::String(s) => parse_local_datetime_text(s).or_else(|| {
            parse_time_text(s).map(|t| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap().and_time(t))
        }),
        _ => None,
    }
}

pub(super) fn base_time(value: &Value) -> Option<NaiveTime> {
    match value {
        Value::LocalTime(n) | Value::Time { nanos: n, .. } => naive_time(*n),
        Value::String(s) => parse_time(s)
            .ok()
            .and_then(|(nanos, _)| naive_time(nanos))
            .or_else(|| base_datetime(value).map(|t| t.time())),
        _ => base_datetime(value).map(|t| t.time()),
    }
}

pub(super) fn base_date(value: &Value) -> Option<NaiveDate> {
    match value {
        Value::String(s) => parse_date_text(s).or_else(|| base_datetime(value).map(|t| t.date())),
        _ => base_datetime(value).map(|t| t.date()),
    }
}

/// Build the value a `date`/`localtime`/`localdatetime`/`datetime` map names.
/// A null component or base gives null, as `point({...})` does; an unknown
/// key, a non-integer component, an impossible date or clock is an error.
pub(super) fn temporal_from_map(target: MapTarget, map: &PropMap) -> Result<Value, String> {
    let function = target.name();
    if let Some((key, _)) = map.iter().find(|(key, _)| !target.accepts(key)) {
        return Err(format!(
            "{function}(): unknown key '{key}'; accepted keys are {}",
            target.accepted()
        ));
    }
    if map.iter().any(|(_, v)| matches!(v, Value::Null)) {
        return Ok(Value::Null);
    }
    let component = |key: &str| -> Result<Option<i64>, String> {
        match map.get(key) {
            None => Ok(None),
            Some(Value::Int64(n)) => Ok(Some(*n)),
            Some(other) => Err(format!(
                "{function}(): {key} must be an integer, got {}",
                other.type_name()
            )),
        }
    };

    let offset_secs = if target == MapTarget::Time {
        Some(map_offset(function, map)?)
    } else {
        None
    };
    let mut date = None;
    let mut time = None;
    if target.has_date() {
        let year = component("year")?;
        let month = component("month")?;
        let day = component("day")?;
        let week = component("week")?;
        let day_of_week = component("dayOfWeek")?;
        let ordinal = component("ordinalDay")?;
        let quarter = component("quarter")?;
        let day_in_quarter = component("dayOfQuarter")?;
        let datetime_base = map.get("datetime").and_then(base_datetime);
        let date_base = map
            .get("date")
            .and_then(base_date)
            .or_else(|| datetime_base.map(|t| t.date()));
        for key in ["date", "datetime"] {
            if let Some(value) = map.get(key) {
                if base_datetime(value).is_none() && base_date(value).is_none() {
                    return Err(format!(
                        "{function}(): '{key}' must be a date or a datetime, got {}",
                        value.type_name()
                    ));
                }
            }
        }
        let keys = CalendarKeys {
            year,
            month,
            day,
            week,
            day_of_week,
            ordinal,
            quarter,
            day_in_quarter,
        };
        date = Some(build_date(function, keys, date_base)?);
        time = match map.get("time") {
            Some(value) => Some(base_time(value).ok_or_else(|| {
                format!(
                    "{function}(): 'time' must be a time or a datetime, got {}",
                    value.type_name()
                )
            })?),
            None => datetime_base.map(|t| t.time()),
        };
    } else if let Some(value) = map.get("time") {
        time = Some(base_time(value).ok_or_else(|| {
            format!(
                "{function}(): 'time' must be a time or a datetime, got {}",
                value.type_name()
            )
        })?);
    }

    let clock = if target == MapTarget::Date {
        None
    } else {
        let base = time.unwrap_or(NaiveTime::MIN);
        let hour = component("hour")?.unwrap_or(base.hour() as i64);
        let minute = component("minute")?.unwrap_or(base.minute() as i64);
        let second = component("second")?.unwrap_or(base.second() as i64);
        let (ms, us, ns) = (
            component("millisecond")?,
            component("microsecond")?,
            component("nanosecond")?,
        );
        let nanos = if ms.is_none() && us.is_none() && ns.is_none() {
            base.nanosecond() as i64
        } else {
            ms.unwrap_or(0) * 1_000_000 + us.unwrap_or(0) * 1_000 + ns.unwrap_or(0)
        };
        Some((hour, minute, second, nanos))
    };
    let invalid = |date: Option<NaiveDate>| {
        let calendar = date.map_or(String::new(), |d| d.format("%Y-%m-%d").to_string());
        let clock = clock.map_or(String::new(), |(h, m, s, n)| {
            format!(
                "{}{h:02}:{m:02}:{s:02}.{n:09}",
                if date.is_some() { " " } else { "" }
            )
        });
        format!("{function}(): {calendar}{clock} is not a valid {function}")
    };
    let time = match clock {
        None => None,
        Some((hour, minute, second, nanos)) => {
            let part = |n: i64| u32::try_from(n).ok();
            Some(
                part(hour)
                    .zip(part(minute))
                    .zip(part(second))
                    .zip(part(nanos))
                    .and_then(|(((h, m), s), n)| NaiveTime::from_hms_nano_opt(h, m, s, n))
                    .ok_or_else(|| invalid(date))?,
            )
        }
    };
    Ok(match (target, date, time) {
        (MapTarget::Date, Some(date), _) => Value::DateTime(date),
        (MapTarget::LocalTime, _, Some(time)) => Value::LocalTime(nanos_of(time)),
        (MapTarget::Time, _, Some(time)) => Value::Time {
            nanos: nanos_of(time),
            offset_secs: offset_secs.unwrap_or(0),
        },
        (_, Some(date), Some(time)) => Value::Timestamp(date.and_time(time)),
        _ => Value::Null,
    })
}

/// The offset of a `time({...})` map: its `timezone`, else the offset of a
/// `time` base that has one, else UTC.
fn map_offset(function: &str, map: &PropMap) -> Result<i32, String> {
    match map.get("timezone") {
        Some(Value::String(zone)) => parse_offset(zone).ok_or_else(|| {
            format!(
                "{function}(): timezone '{zone}' is not a numeric offset; named time zones are not supported on time values"
            )
        }),
        Some(other) => Err(format!(
            "{function}(): timezone must be a string, got {}",
            other.type_name()
        )),
        None => Ok(match map.get("time") {
            Some(Value::Time { offset_secs, .. }) => *offset_secs,
            Some(Value::String(text)) => parse_time(text)
                .ok()
                .and_then(|(_, offset)| offset)
                .unwrap_or(0),
            _ => 0,
        }),
    }
}

/// The calendar components a map named, before any base fills the rest.
struct CalendarKeys {
    year: Option<i64>,
    month: Option<i64>,
    day: Option<i64>,
    week: Option<i64>,
    day_of_week: Option<i64>,
    ordinal: Option<i64>,
    quarter: Option<i64>,
    day_in_quarter: Option<i64>,
}

fn build_date(
    function: &str,
    keys: CalendarKeys,
    base: Option<NaiveDate>,
) -> Result<NaiveDate, String> {
    let CalendarKeys {
        year,
        month,
        day,
        week,
        day_of_week,
        ordinal,
        quarter,
        day_in_quarter,
    } = keys;
    let narrow = |n: i64| i32::try_from(n).map_err(|_| format!("{function}(): {n} out of range"));
    let wrap = |e: String| format!("{function}(): {e}");
    if week.is_some() || (day_of_week.is_some() && ordinal.is_none() && quarter.is_none()) {
        let year = match (year, base) {
            (Some(y), _) => y,
            (None, Some(b)) => b.iso_week().year() as i64,
            (None, None) => return Err(format!("{function}(): missing 'year'")),
        };
        let week = week
            .or_else(|| base.map(|b| b.iso_week().week() as i64))
            .unwrap_or(1);
        let dow = day_of_week
            .or_else(|| base.map(|b| b.weekday().number_from_monday() as i64))
            .unwrap_or(1);
        return date_from_iso_week(narrow(year)?, week, dow).map_err(wrap);
    }
    if let Some(ordinal) = ordinal {
        let year = year
            .or_else(|| base.map(|b| b.year() as i64))
            .ok_or_else(|| format!("{function}(): missing 'year'"))?;
        return date_from_ordinal(narrow(year)?, ordinal).map_err(wrap);
    }
    if quarter.is_some() || day_in_quarter.is_some() {
        let year = year
            .or_else(|| base.map(|b| b.year() as i64))
            .ok_or_else(|| format!("{function}(): missing 'year'"))?;
        let quarter = quarter
            .or_else(|| base.map(|b| (b.month0() / 3 + 1) as i64))
            .unwrap_or(1);
        let dq = day_in_quarter
            .or_else(|| base.map(day_of_quarter))
            .unwrap_or(1);
        return date_from_quarter(narrow(year)?, quarter, dq).map_err(wrap);
    }
    let year = year
        .or_else(|| base.map(|b| b.year() as i64))
        .ok_or_else(|| format!("{function}(): missing 'year'"))?;
    let month = month
        .or_else(|| base.map(|b| b.month() as i64))
        .unwrap_or(1);
    let day = day.or_else(|| base.map(|b| b.day() as i64)).unwrap_or(1);
    i32::try_from(year)
        .ok()
        .zip(u32::try_from(month).ok())
        .zip(u32::try_from(day).ok())
        .and_then(|((y, m), d)| NaiveDate::from_ymd_opt(y, m, d))
        .ok_or_else(|| {
            format!("{function}(): {year}-{month:02}-{day:02} is not a valid {function}")
        })
}
