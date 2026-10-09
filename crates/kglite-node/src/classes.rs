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

/// A Cypher duration: calendar months and days are kept apart from clock seconds.
#[napi]
pub struct Duration {
    #[napi(readonly)]
    pub months: i32,
    #[napi(readonly)]
    pub days: i32,
    #[napi(readonly)]
    pub seconds: i64,
}

#[napi]
impl Duration {
    #[napi(constructor)]
    pub fn new(
        months: Option<i32>,
        days: Option<i32>,
        seconds: Option<i64>,
    ) -> napi::Result<Self, &'static str> {
        contain(|| {
            Ok(Self {
                months: months.unwrap_or(0),
                days: days.unwrap_or(0),
                seconds: seconds.unwrap_or(0),
            })
        })
    }

    /// ISO-8601 (`P1Y2M3DT4S`); zero parts are omitted and a zero duration is `PT0S`.
    #[napi(js_name = "toString")]
    pub fn to_iso(&self) -> napi::Result<String, &'static str> {
        contain(|| Ok(iso_duration(self.months, self.days, self.seconds)))
    }

    #[napi(
        js_name = "toJSON",
        ts_return_type = "{ months: number; days: number; seconds: number }"
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
            Ok(o)
        })
    }
}

pub fn iso_duration(months: i32, days: i32, seconds: i64) -> String {
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
    if seconds != 0 || out == "P" {
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
        assert_eq!(iso_duration(0, 0, 0), "PT0S");
        assert_eq!(iso_duration(14, 3, 4), "P1Y2M3DT4S");
        assert_eq!(iso_duration(1, 0, 0), "P1M");
        assert_eq!(iso_duration(-1, -2, -3), "P-1M-2DT-3S");
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
