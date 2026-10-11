//! Small exact value classes for the types JavaScript has no exact native for.
//!
//! Immutable, with ISO `toString()` and a `toJSON()` that matches the engine's
//! JSON rendering (`kglite_value_to_json`), so `JSON.stringify(result)` is safe.

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
use napi::bindgen_prelude::Object;
use napi::Env;
use napi_derive::napi;

use crate::contain;
use crate::errors::{to_sync_error, JsErr};

fn bad(message: String) -> napi::Error<&'static str> {
    to_sync_error(JsErr::arg(message))
}

/// A calendar date without a time or zone.
#[napi]
pub struct LocalDate {
    #[napi(readonly)]
    pub year: i32,
    #[napi(readonly)]
    pub month: u32,
    #[napi(readonly)]
    pub day: u32,
}

impl LocalDate {
    pub fn to_naive(&self) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.year, self.month, self.day).expect("validated at construction")
    }
    pub fn from_naive(d: NaiveDate) -> Self {
        Self {
            year: d.year(),
            month: d.month(),
            day: d.day(),
        }
    }
}

#[napi]
impl LocalDate {
    #[napi(constructor)]
    pub fn new(year: i32, month: u32, day: u32) -> napi::Result<Self, &'static str> {
        contain(|| {
            NaiveDate::from_ymd_opt(year, month, day)
                .map(|_| Self { year, month, day })
                .ok_or_else(|| bad(format!("invalid date {year}-{month}-{day}")))
        })
    }

    /// ISO-8601 `YYYY-MM-DD`.
    #[napi(js_name = "toString")]
    pub fn to_iso(&self) -> napi::Result<String, &'static str> {
        contain(|| Ok(self.to_naive().format("%Y-%m-%d").to_string()))
    }

    #[napi(js_name = "toJSON")]
    pub fn to_json(&self) -> napi::Result<String, &'static str> {
        self.to_iso()
    }
}

/// A calendar date with a wall-clock time (nanosecond precision), no zone.
#[napi]
pub struct LocalDateTime {
    #[napi(readonly)]
    pub year: i32,
    #[napi(readonly)]
    pub month: u32,
    #[napi(readonly)]
    pub day: u32,
    #[napi(readonly)]
    pub hour: u32,
    #[napi(readonly)]
    pub minute: u32,
    #[napi(readonly)]
    pub second: u32,
    #[napi(readonly)]
    pub nanosecond: u32,
}

impl LocalDateTime {
    pub fn to_naive(&self) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(self.year, self.month, self.day)
            .and_then(|d| d.and_hms_nano_opt(self.hour, self.minute, self.second, self.nanosecond))
            .expect("validated at construction")
    }
    pub fn from_naive(t: NaiveDateTime) -> Self {
        Self {
            year: t.year(),
            month: t.month(),
            day: t.day(),
            hour: t.hour(),
            minute: t.minute(),
            second: t.second(),
            // chrono encodes a leap second as nanosecond >= 1e9; keep it exact.
            nanosecond: t.nanosecond(),
        }
    }
}

#[napi]
impl LocalDateTime {
    #[napi(constructor)]
    pub fn new(
        year: i32,
        month: u32,
        day: u32,
        hour: Option<u32>,
        minute: Option<u32>,
        second: Option<u32>,
        nanosecond: Option<u32>,
    ) -> napi::Result<Self, &'static str> {
        contain(|| {
            let (hour, minute, second, nanosecond) = (
                hour.unwrap_or(0),
                minute.unwrap_or(0),
                second.unwrap_or(0),
                nanosecond.unwrap_or(0),
            );
            NaiveDate::from_ymd_opt(year, month, day)
                .and_then(|d| d.and_hms_nano_opt(hour, minute, second, nanosecond))
                .map(|_| Self {
                    year,
                    month,
                    day,
                    hour,
                    minute,
                    second,
                    nanosecond,
                })
                .ok_or_else(|| {
                    bad(format!(
                        "invalid datetime {year}-{month}-{day} {hour}:{minute}:{second}.{nanosecond}"
                    ))
                })
        })
    }

    /// ISO-8601 with the shortest of 0, 3, 6 or 9 fractional digits.
    #[napi(js_name = "toString")]
    pub fn to_iso(&self) -> napi::Result<String, &'static str> {
        contain(|| Ok(self.to_naive().format("%Y-%m-%dT%H:%M:%S%.f").to_string()))
    }

    /// A JS `Date` reading this wall clock as UTC; sub-millisecond digits are dropped.
    #[napi(js_name = "toDate", ts_return_type = "Date")]
    pub fn to_date<'e>(&self, env: &'e Env) -> napi::Result<napi::JsDate<'e>, &'static str> {
        contain(|| {
            let ms = self.to_naive().and_utc().timestamp_millis() as f64;
            env.create_date(ms)
                .map_err(|e| to_sync_error(JsErr::from(e)))
        })
    }

    #[napi(js_name = "toJSON")]
    pub fn to_json(&self) -> napi::Result<String, &'static str> {
        self.to_iso()
    }
}

/// An instant with the UTC offset it was written in, to the nanosecond. The fields
/// read the wall clock at that offset. Two datetimes at the same instant are equal
/// in the engine whatever their offsets.
#[napi]
pub struct ZonedDateTime {
    #[napi(readonly)]
    pub year: i32,
    #[napi(readonly)]
    pub month: u32,
    #[napi(readonly)]
    pub day: u32,
    #[napi(readonly)]
    pub hour: u32,
    #[napi(readonly)]
    pub minute: u32,
    #[napi(readonly)]
    pub second: u32,
    #[napi(readonly)]
    pub nanosecond: u32,
    /// Seconds east of UTC.
    #[napi(readonly)]
    pub offset_seconds: i32,
}

impl ZonedDateTime {
    /// The UTC instant and offset an engine value carries.
    pub fn to_parts(&self) -> (NaiveDateTime, i32) {
        let wall = NaiveDate::from_ymd_opt(self.year, self.month, self.day)
            .and_then(|d| d.and_hms_nano_opt(self.hour, self.minute, self.second, self.nanosecond))
            .expect("validated at construction");
        let utc = kglite::api::zoned_datetime::utc_from_wall(wall, self.offset_seconds)
            .expect("validated at construction");
        (utc, self.offset_seconds)
    }
    pub fn from_parts(utc: NaiveDateTime, offset_seconds: i32) -> Self {
        let wall = kglite::api::zoned_datetime::wall_clock(utc, offset_seconds);
        Self {
            year: wall.year(),
            month: wall.month(),
            day: wall.day(),
            hour: wall.hour(),
            minute: wall.minute(),
            second: wall.second(),
            nanosecond: wall.nanosecond(),
            offset_seconds,
        }
    }
}

#[napi]
impl ZonedDateTime {
    /// The wall clock at `offsetSeconds` (default 0, UTC).
    // The JS constructor takes one wall-clock reading plus its offset, so the
    // eight positional parameters are the public signature.
    #[allow(clippy::too_many_arguments)]
    #[napi(constructor)]
    pub fn new(
        year: i32,
        month: u32,
        day: u32,
        hour: Option<u32>,
        minute: Option<u32>,
        second: Option<u32>,
        nanosecond: Option<u32>,
        offset_seconds: Option<i32>,
    ) -> napi::Result<Self, &'static str> {
        contain(|| {
            let (hour, minute, second, nanosecond, offset_seconds) = (
                hour.unwrap_or(0),
                minute.unwrap_or(0),
                second.unwrap_or(0),
                nanosecond.unwrap_or(0),
                offset_seconds.unwrap_or(0),
            );
            if offset_seconds.abs() > kglite::api::time_of_day::MAX_OFFSET_SECS {
                return Err(bad(format!(
                    "invalid offset {offset_seconds}s: a datetime's offset is within ±18 hours"
                )));
            }
            NaiveDate::from_ymd_opt(year, month, day)
                .and_then(|d| d.and_hms_nano_opt(hour, minute, second, nanosecond))
                .filter(|wall| {
                    kglite::api::zoned_datetime::utc_from_wall(*wall, offset_seconds).is_some()
                })
                .map(|_| Self {
                    year,
                    month,
                    day,
                    hour,
                    minute,
                    second,
                    nanosecond,
                    offset_seconds,
                })
                .ok_or_else(|| {
                    bad(format!(
                        "invalid datetime {year}-{month}-{day} {hour}:{minute}:{second}.{nanosecond}"
                    ))
                })
        })
    }

    /// ISO-8601 with the offset: `2024-02-29T12:30:00.5+01:00`, `…Z` for UTC.
    #[napi(js_name = "toString")]
    pub fn to_iso(&self) -> napi::Result<String, &'static str> {
        contain(|| {
            let (utc, offset) = self.to_parts();
            Ok(kglite::api::zoned_datetime::format_zoned(utc, offset))
        })
    }

    /// The JS `Date` of this instant; sub-millisecond digits are dropped.
    #[napi(js_name = "toDate", ts_return_type = "Date")]
    pub fn to_date<'e>(&self, env: &'e Env) -> napi::Result<napi::JsDate<'e>, &'static str> {
        contain(|| {
            let ms = self.to_parts().0.and_utc().timestamp_millis() as f64;
            env.create_date(ms)
                .map_err(|e| to_sync_error(JsErr::from(e)))
        })
    }

    #[napi(js_name = "toJSON")]
    pub fn to_json(&self) -> napi::Result<String, &'static str> {
        self.to_iso()
    }
}

/// A time of day with no offset, to the nanosecond.
#[napi]
pub struct LocalTime {
    #[napi(readonly)]
    pub hour: u32,
    #[napi(readonly)]
    pub minute: u32,
    #[napi(readonly)]
    pub second: u32,
    #[napi(readonly)]
    pub nanosecond: u32,
}

/// Nanoseconds since midnight for a validated clock reading, `None` when the
/// reading is not a time of day (hour 24, minute 60, a leap second, …).
fn clock_nanos(hour: u32, minute: u32, second: u32, nanosecond: u32) -> Option<u64> {
    (hour < 24 && minute < 60 && second < 60 && nanosecond < 1_000_000_000).then(|| {
        (u64::from(hour) * 3600 + u64::from(minute) * 60 + u64::from(second)) * 1_000_000_000
            + u64::from(nanosecond)
    })
}

impl LocalTime {
    pub fn to_nanos(&self) -> u64 {
        clock_nanos(self.hour, self.minute, self.second, self.nanosecond)
            .expect("validated at construction")
    }
    pub fn from_nanos(nanos: u64) -> Self {
        let secs = nanos / 1_000_000_000;
        Self {
            hour: (secs / 3600) as u32,
            minute: (secs / 60 % 60) as u32,
            second: (secs % 60) as u32,
            nanosecond: (nanos % 1_000_000_000) as u32,
        }
    }
}

#[napi]
impl LocalTime {
    #[napi(constructor)]
    pub fn new(
        hour: u32,
        minute: Option<u32>,
        second: Option<u32>,
        nanosecond: Option<u32>,
    ) -> napi::Result<Self, &'static str> {
        contain(|| {
            let (minute, second, nanosecond) = (
                minute.unwrap_or(0),
                second.unwrap_or(0),
                nanosecond.unwrap_or(0),
            );
            clock_nanos(hour, minute, second, nanosecond)
                .map(|_| Self {
                    hour,
                    minute,
                    second,
                    nanosecond,
                })
                .ok_or_else(|| {
                    bad(format!(
                        "invalid time {hour}:{minute}:{second}.{nanosecond}"
                    ))
                })
        })
    }

    /// ISO-8601 with seconds omitted when zero and the shortest of 0, 3, 6 or 9 fractional digits.
    #[napi(js_name = "toString")]
    pub fn to_iso(&self) -> napi::Result<String, &'static str> {
        contain(|| Ok(kglite::api::time_of_day::format_clock(self.to_nanos())))
    }

    #[napi(js_name = "toJSON")]
    pub fn to_json(&self) -> napi::Result<String, &'static str> {
        self.to_iso()
    }
}

/// A time of day with a UTC offset (whole seconds, within ±18 hours), to the nanosecond.
/// Two times at the same instant of day are equal in the engine whatever their offsets.
#[napi]
pub struct Time {
    #[napi(readonly)]
    pub hour: u32,
    #[napi(readonly)]
    pub minute: u32,
    #[napi(readonly)]
    pub second: u32,
    #[napi(readonly)]
    pub nanosecond: u32,
    /// Seconds east of UTC.
    #[napi(readonly)]
    pub offset_seconds: i32,
}

impl Time {
    pub fn to_nanos(&self) -> u64 {
        clock_nanos(self.hour, self.minute, self.second, self.nanosecond)
            .expect("validated at construction")
    }
    pub fn from_parts(nanos: u64, offset_seconds: i32) -> Self {
        let local = LocalTime::from_nanos(nanos);
        Self {
            hour: local.hour,
            minute: local.minute,
            second: local.second,
            nanosecond: local.nanosecond,
            offset_seconds,
        }
    }
}

#[napi]
impl Time {
    #[napi(constructor)]
    pub fn new(
        hour: u32,
        minute: Option<u32>,
        second: Option<u32>,
        nanosecond: Option<u32>,
        offset_seconds: Option<i32>,
    ) -> napi::Result<Self, &'static str> {
        contain(|| {
            let (minute, second, nanosecond, offset_seconds) = (
                minute.unwrap_or(0),
                second.unwrap_or(0),
                nanosecond.unwrap_or(0),
                offset_seconds.unwrap_or(0),
            );
            if offset_seconds.abs() > kglite::api::time_of_day::MAX_OFFSET_SECS {
                return Err(bad(format!(
                    "invalid offset {offset_seconds}s: a time's offset is within ±18 hours"
                )));
            }
            clock_nanos(hour, minute, second, nanosecond)
                .map(|_| Self {
                    hour,
                    minute,
                    second,
                    nanosecond,
                    offset_seconds,
                })
                .ok_or_else(|| {
                    bad(format!(
                        "invalid time {hour}:{minute}:{second}.{nanosecond}"
                    ))
                })
        })
    }

    /// ISO-8601 with the offset: `10:00:00.5+01:00`, `10:00Z`.
    #[napi(js_name = "toString")]
    pub fn to_iso(&self) -> napi::Result<String, &'static str> {
        contain(|| {
            Ok(kglite::api::time_of_day::format_time(
                self.to_nanos(),
                self.offset_seconds,
            ))
        })
    }

    #[napi(js_name = "toJSON")]
    pub fn to_json(&self) -> napi::Result<String, &'static str> {
        self.to_iso()
    }
}

/// A Cypher duration: calendar months and days are kept apart from clock seconds.
#[napi]
pub struct Duration {
    #[napi(readonly)]
    pub months: i32,
    #[napi(readonly)]
    pub days: i32,
    #[napi(readonly)]
    pub seconds: i64,
    /// Sub-second part, 0 to 999999999, added to the (signed) `seconds`.
    #[napi(readonly)]
    pub nanoseconds: i32,
}

#[napi]
impl Duration {
    #[napi(constructor)]
    pub fn new(
        months: Option<i32>,
        days: Option<i32>,
        seconds: Option<i64>,
        nanoseconds: Option<i32>,
    ) -> napi::Result<Self, &'static str> {
        contain(|| {
            let nanoseconds = nanoseconds.unwrap_or(0);
            if !(0..1_000_000_000).contains(&nanoseconds) {
                return Err(bad("Duration nanoseconds must be in 0..999999999".into()));
            }
            Ok(Self {
                months: months.unwrap_or(0),
                days: days.unwrap_or(0),
                seconds: seconds.unwrap_or(0),
                nanoseconds,
            })
        })
    }

    /// ISO-8601 (`P1Y2M3DT4S`); zero parts are omitted and a zero duration is `PT0S`.
    #[napi(js_name = "toString")]
    pub fn to_iso(&self) -> napi::Result<String, &'static str> {
        contain(|| {
            Ok(iso_duration(
                self.months,
                self.days,
                self.seconds,
                self.nanoseconds,
            ))
        })
    }

    #[napi(
        js_name = "toJSON",
        ts_return_type = "{ months: number; days: number; seconds: number; nanoseconds?: number }"
    )]
    pub fn to_json<'e>(&self, env: &'e Env) -> napi::Result<Object<'e>, &'static str> {
        contain(|| {
            let mut o = Object::new(env).map_err(|e| to_sync_error(JsErr::from(e)))?;
            let set = |o: &mut Object, k: &str, v: f64| {
                o.set(k, v).map_err(|e| to_sync_error(JsErr::from(e)))
            };
            set(&mut o, "months", f64::from(self.months))?;
            set(&mut o, "days", f64::from(self.days))?;
            set(&mut o, "seconds", self.seconds as f64)?;
            if self.nanoseconds != 0 {
                set(&mut o, "nanoseconds", f64::from(self.nanoseconds))?;
            }
            Ok(o)
        })
    }
}

/// ISO text for a duration. The seconds field is `seconds + nanoseconds / 1e9`,
/// so a negative total with a fraction prints as one signed decimal.
pub fn iso_duration(months: i32, days: i32, seconds: i64, nanoseconds: i32) -> String {
    let (years, months) = (months / 12, months % 12);
    let mut out = String::from("P");
    for (n, unit) in [
        (i64::from(years), 'Y'),
        (i64::from(months), 'M'),
        (i64::from(days), 'D'),
    ] {
        if n != 0 {
            out.push_str(&format!("{n}{unit}"));
        }
    }
    if nanoseconds != 0 {
        let (sign, whole, frac) = if seconds < 0 {
            ("-", -(seconds + 1), 1_000_000_000 - nanoseconds)
        } else {
            ("", seconds, nanoseconds)
        };
        let frac = format!("{frac:09}");
        out.push_str(&format!("T{sign}{whole}.{}S", frac.trim_end_matches('0')));
    } else if seconds != 0 || out == "P" {
        out.push_str(&format!("T{seconds}S"));
    }
    out
}

/// A WGS-84 point.
#[napi]
pub struct Point {
    #[napi(readonly)]
    pub latitude: f64,
    #[napi(readonly)]
    pub longitude: f64,
}

#[napi]
impl Point {
    #[napi(constructor)]
    pub fn new(latitude: f64, longitude: f64) -> napi::Result<Self, &'static str> {
        contain(|| {
            Ok(Self {
                latitude,
                longitude,
            })
        })
    }

    /// WKT, `POINT(longitude latitude)`.
    #[napi(js_name = "toString")]
    pub fn to_wkt(&self) -> napi::Result<String, &'static str> {
        contain(|| Ok(format!("POINT({} {})", self.longitude, self.latitude)))
    }

    #[napi(
        js_name = "toJSON",
        ts_return_type = "{ latitude: number; longitude: number }"
    )]
    pub fn to_json<'e>(&self, env: &'e Env) -> napi::Result<Object<'e>, &'static str> {
        contain(|| {
            let mut o = Object::new(env).map_err(|e| to_sync_error(JsErr::from(e)))?;
            o.set("latitude", self.latitude)
                .and_then(|()| o.set("longitude", self.longitude))
                .map_err(|e| to_sync_error(JsErr::from(e)))?;
            Ok(o)
        })
    }
}

/// Forces a parameter to the engine's float type. A plain JS `1` is an integer
/// because JavaScript cannot tell `1` from `1.0`.
#[napi]
pub struct KgFloat {
    #[napi(readonly)]
    pub value: f64,
}

#[napi]
impl KgFloat {
    #[napi(constructor)]
    pub fn new(value: f64) -> napi::Result<Self, &'static str> {
        contain(|| Ok(Self { value }))
    }

    #[napi(js_name = "toJSON")]
    pub fn to_json(&self) -> napi::Result<f64, &'static str> {
        contain(|| Ok(self.value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_duration_shapes() {
        assert_eq!(iso_duration(0, 0, 0, 0), "PT0S");
        assert_eq!(iso_duration(14, 3, 4, 0), "P1Y2M3DT4S");
        assert_eq!(iso_duration(1, 0, 0, 0), "P1M");
        assert_eq!(iso_duration(-1, -2, -3, 0), "P-1M-2DT-3S");
        assert_eq!(iso_duration(0, 0, 1, 500_000_000), "PT1.5S");
        assert_eq!(iso_duration(0, 0, -2, 500_000_000), "PT-1.5S");
        assert_eq!(iso_duration(0, 1, 0, 1), "P1DT0.000000001S");
    }

    #[test]
    fn datetime_round_trips_nanoseconds() {
        let t = NaiveDate::from_ymd_opt(2024, 2, 29)
            .unwrap()
            .and_hms_nano_opt(23, 59, 58, 123_456_789)
            .unwrap();
        assert_eq!(LocalDateTime::from_naive(t).to_naive(), t);
    }
}
