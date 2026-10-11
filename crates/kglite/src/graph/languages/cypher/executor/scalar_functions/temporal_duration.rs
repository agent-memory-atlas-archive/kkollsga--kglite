//! Durations: building one from component maps (fractions of a larger unit
//! spill into the next smaller one, as in Neo4j) and from ISO 8601 text, and
//! rendering one as ISO 8601 text. A duration is whole months, whole days and
//! seconds with a nanosecond remainder; a fraction finer than one nanosecond
//! is rounded to the nearest nanosecond, as Neo4j does.
use super::super::*;
use super::shared::coerce_naive_datetime;
use crate::datatypes::values::Value;
use crate::datatypes::PropMap;
use chrono::{Datelike, NaiveDateTime};

/// Mean length of a month in days, as Neo4j converts a fractional month.
const DAYS_PER_MONTH: f64 = 30.436875;

/// Exact integer totals plus the fractional remainders still to be carried
/// into the next smaller unit (months into days, days into seconds).
#[derive(Default)]
struct Components {
    months: i64,
    days: i64,
    seconds: i64,
    months_fraction: f64,
    days_fraction: f64,
    seconds_fraction: f64,
    /// Integer milli-, micro- and nanoseconds, exact so that one nanosecond
    /// is never rounded away.
    nanos: i128,
}

fn checked_scaled_add(
    total: &mut i64,
    value: i64,
    scale: i64,
    component: &str,
) -> Result<(), String> {
    let scaled = value
        .checked_mul(scale)
        .ok_or_else(|| format!("duration() {component} component overflow"))?;
    *total = total
        .checked_add(scaled)
        .ok_or_else(|| format!("duration() {component} component overflow"))?;
    Ok(())
}

impl Components {
    fn add(&mut self, key: &str, n: f64, exact: Option<i64>) -> Result<(), String> {
        if !n.is_finite() {
            return Err(format!(
                "duration(): '{key}' must be a whole number of {key}, got {n}"
            ));
        }
        // Whole part exactly (an integer argument never goes through f64),
        // fractional remainder in `n`'s own unit.
        let (whole, fraction) = match exact {
            Some(v) => (v, 0.0),
            None if n.abs() < 9.0e18 => (n.trunc() as i64, n - n.trunc()),
            None => return Err(format!("duration() {key} component overflow")),
        };
        match key {
            "years" => {
                checked_scaled_add(&mut self.months, whole, 12, key)?;
                self.months_fraction += fraction * 12.0;
            }
            "months" => {
                checked_scaled_add(&mut self.months, whole, 1, key)?;
                self.months_fraction += fraction;
            }
            "weeks" => {
                checked_scaled_add(&mut self.days, whole, 7, key)?;
                self.days_fraction += fraction * 7.0;
            }
            "days" => {
                checked_scaled_add(&mut self.days, whole, 1, key)?;
                self.days_fraction += fraction;
            }
            "hours" => {
                checked_scaled_add(&mut self.seconds, whole, 3600, key)?;
                self.seconds_fraction += fraction * 3600.0;
            }
            "minutes" => {
                checked_scaled_add(&mut self.seconds, whole, 60, key)?;
                self.seconds_fraction += fraction * 60.0;
            }
            "seconds" => {
                checked_scaled_add(&mut self.seconds, whole, 1, key)?;
                self.seconds_fraction += fraction;
            }
            "milliseconds" | "microseconds" | "nanoseconds" => {
                let scale: i128 = match key {
                    "milliseconds" => 1_000_000,
                    "microseconds" => 1_000,
                    _ => 1,
                };
                match exact {
                    Some(v) => self.nanos += v as i128 * scale,
                    None => self.seconds_fraction += n * scale as f64 / 1e9,
                }
            }
            other => {
                return Err(format!(
                    "duration(): unknown key '{other}' (expected years/months/weeks/days/hours/minutes/seconds/milliseconds/microseconds/nanoseconds)"
                ))
            }
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Value, String> {
        let whole_months = self.months_fraction.trunc();
        checked_scaled_add(&mut self.months, whole_months as i64, 1, "months")?;
        self.days_fraction += (self.months_fraction - whole_months) * DAYS_PER_MONTH;
        let whole_days = self.days_fraction.trunc();
        checked_scaled_add(&mut self.days, whole_days as i64, 1, "days")?;
        self.seconds_fraction += (self.days_fraction - whole_days) * 86_400.0;
        // Fractional seconds are f64 noise-prone past the nanosecond, so they
        // join the exact integer nanoseconds only after rounding to one.
        let fraction_nanos = (self.seconds_fraction * 1e9).round();
        if !fraction_nanos.is_finite() || fraction_nanos.abs() > 1e24 {
            return Err("duration() seconds component overflow".to_string());
        }
        let total_nanos = self.nanos + fraction_nanos as i128;
        let whole_nanos = total_nanos.div_euclid(1_000_000_000);
        let nanos = total_nanos.rem_euclid(1_000_000_000) as i32;
        checked_scaled_add(
            &mut self.seconds,
            i64::try_from(whole_nanos).map_err(|_| "duration() seconds component overflow")?,
            1,
            "seconds",
        )?;
        Ok(Value::Duration {
            months: i32::try_from(self.months)
                .map_err(|_| "duration() calendar months exceed the supported i32 range")?,
            days: i32::try_from(self.days)
                .map_err(|_| "duration() calendar days exceed the supported i32 range")?,
            seconds: self.seconds,
            nanos,
        })
    }
}

/// `duration({years, months, weeks, days, hours, minutes, seconds,
/// milliseconds, microseconds, nanoseconds})`. A null component counts as 0.
pub(super) fn duration_from_map(map: &PropMap) -> Result<Value, String> {
    let mut parts = Components::default();
    for (key, value) in map.iter() {
        match value {
            Value::Int64(n) => parts.add(key, *n as f64, Some(*n))?,
            Value::Float64(f) => parts.add(key, *f, None)?,
            Value::Null => {}
            other => {
                return Err(format!(
                    "duration(): '{key}' must be a number of {key}, got {}",
                    other.type_name()
                ))
            }
        }
    }
    parts.finish()
}

/// `duration('P14DT16H12M')`, fractions allowed on any unit, and the
/// alternative `P2012-02-02T14:37:21` form.
pub(super) fn duration_from_text(text: &str) -> Result<Value, String> {
    let bad = || format!("duration(): cannot parse '{text}' as an ISO-8601 duration");
    let body = text.trim().strip_prefix('P').ok_or_else(bad)?;
    let mut parts = Components::default();
    if let Some(done) = alternative_form(body, &mut parts).map_err(|_| bad())? {
        if done {
            return parts.finish();
        }
    }
    let (date, time) = match body.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (body, None),
    };
    let mut any = false;
    for (section, units) in [
        (
            Some(date),
            &[
                ('Y', "years"),
                ('M', "months"),
                ('W', "weeks"),
                ('D', "days"),
            ][..],
        ),
        (
            time,
            &[('H', "hours"), ('M', "minutes"), ('S', "seconds")][..],
        ),
    ] {
        let Some(section) = section else { continue };
        let mut rest = section;
        let mut last = None;
        while !rest.is_empty() {
            let end = rest
                .find(|c: char| c.is_ascii_alphabetic())
                .ok_or_else(bad)?;
            let number: f64 = rest[..end].parse().map_err(|_| bad())?;
            let designator = rest[end..].chars().next().ok_or_else(bad)?;
            let position = units
                .iter()
                .position(|(d, _)| *d == designator)
                .ok_or_else(bad)?;
            if last.is_some_and(|l| l >= position) {
                return Err(bad());
            }
            last = Some(position);
            parts.add(units[position].1, number, None)?;
            any = true;
            rest = &rest[end + designator.len_utf8()..];
        }
    }
    if !any {
        return Err(bad());
    }
    parts.finish()
}

/// `YYYY-MM-DDThh:mm:ss[.f]` after the `P`: `Ok(Some(true))` when it matched.
fn alternative_form(body: &str, parts: &mut Components) -> Result<Option<bool>, ()> {
    let Some((date, time)) = body.split_once('T') else {
        return Ok(None);
    };
    let d: Vec<&str> = date.split('-').collect();
    let t: Vec<&str> = time.split(':').collect();
    if d.len() != 3 || t.len() != 3 || !date.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return Ok(None);
    }
    let n = |s: &str| s.parse::<f64>().map_err(|_| ());
    parts.add("years", n(d[0])?, None).map_err(|_| ())?;
    parts.add("months", n(d[1])?, None).map_err(|_| ())?;
    parts.add("days", n(d[2])?, None).map_err(|_| ())?;
    parts.add("hours", n(t[0])?, None).map_err(|_| ())?;
    parts.add("minutes", n(t[1])?, None).map_err(|_| ())?;
    parts.add("seconds", n(t[2])?, None).map_err(|_| ())?;
    Ok(Some(true))
}

/// ISO 8601 text for a duration: `P1Y2M3DT4H5M6.5S`, each component carrying
/// its own sign, `PT0S` for the zero duration. The clock part (seconds plus
/// nanoseconds) is one signed quantity, so `-0.5 s` renders `PT-0.5S`; the
/// fraction keeps only the digits it needs.
pub(crate) fn format_duration_iso(months: i32, days: i32, seconds: i64, nanos: i32) -> String {
    use crate::graph::core::value_operations::clock_total;
    const NANOS: u128 = 1_000_000_000;
    let years = months / 12;
    let months = months % 12;
    let total = clock_total(seconds, nanos);
    let sign = if total < 0 { -1i128 } else { 1 };
    let abs = total.unsigned_abs();
    let hours = (abs / (3600 * NANOS)) as i128 * sign;
    let minutes = ((abs / (60 * NANOS)) % 60) as i128 * sign;
    let in_minute = abs % (60 * NANOS);
    let secs = (in_minute / NANOS) as i128 * sign;
    let fraction = in_minute % NANOS;
    let mut out = String::from("P");
    for (n, unit) in [
        (years as i64, 'Y'),
        (months as i64, 'M'),
        (days as i64, 'D'),
    ] {
        if n != 0 {
            out.push_str(&format!("{n}{unit}"));
        }
    }
    if hours != 0 || minutes != 0 || secs != 0 || fraction != 0 {
        out.push('T');
        for (n, unit) in [(hours, 'H'), (minutes, 'M')] {
            if n != 0 {
                out.push_str(&format!("{n}{unit}"));
            }
        }
        if secs != 0 || fraction != 0 {
            if sign < 0 && secs == 0 {
                out.push('-');
            }
            out.push_str(&secs.to_string());
            if fraction != 0 {
                let digits = format!("{fraction:09}");
                out.push('.');
                out.push_str(digits.trim_end_matches('0'));
            }
            out.push('S');
        }
    }
    if out == "P" {
        out.push_str("T0S");
    }
    out
}

/// Whole calendar months from `start` to `end` (negative when `end` is
/// earlier): the month count, less one when the day and time of day of the
/// later value have not yet reached those of the earlier one.
fn whole_months_between(start: NaiveDateTime, end: NaiveDateTime) -> i64 {
    let months = (end.year() as i64 - start.year() as i64) * 12
        + (end.month() as i64 - start.month() as i64);
    let key = |t: NaiveDateTime| (t.day(), t.time());
    if months > 0 && key(end) < key(start) {
        months - 1
    } else if months < 0 && key(end) > key(start) {
        months + 1
    } else {
        months
    }
}

impl CypherExecutor<'_> {
    /// `duration.inMonths`, `duration.inDays` and `duration.inSeconds`
    /// (`name` is the lowercased dispatcher name): the whole months, days or
    /// seconds from the first date or datetime to the second, as a duration
    /// holding only that unit.
    pub(super) fn eval_duration_in(
        &self,
        name: &str,
        args: &[Expression],
        row: &ResultRow,
    ) -> Result<Value, String> {
        if args.len() != 2 {
            return Err(format!("{name}() requires 2 date or datetime arguments"));
        }
        let a = self.evaluate_expression(&args[0], row)?;
        let b = self.evaluate_expression(&args[1], row)?;
        if matches!(a, Value::Null) || matches!(b, Value::Null) {
            return Ok(Value::Null);
        }
        let (Some(start), Some(end)) = (coerce_naive_datetime(&a), coerce_naive_datetime(&b))
        else {
            return Err(format!("{name}() arguments must be dates or datetimes"));
        };
        let delta = end - start;
        let seconds = delta.num_seconds();
        let narrow = |n: i64| {
            i32::try_from(n).map_err(|_| format!("{name}() result exceeds the supported range"))
        };
        Ok(match name {
            "duration.inmonths" => Value::Duration {
                months: narrow(whole_months_between(start, end))?,
                days: 0,
                seconds: 0,
                nanos: 0,
            },
            "duration.indays" => Value::Duration {
                months: 0,
                days: narrow(seconds / 86_400)?,
                seconds: 0,
                nanos: 0,
            },
            _ => {
                // `num_seconds` truncates toward zero and `subsec_nanos` shares
                // its sign; fold both into the non-negative-nanos form.
                let (seconds, nanos) = crate::graph::core::value_operations::split_clock(
                    i128::from(seconds) * 1_000_000_000 + i128::from(delta.subsec_nanos()),
                )
                .ok_or_else(|| format!("{name}() result exceeds the supported range"))?;
                Value::Duration {
                    months: 0,
                    days: 0,
                    seconds,
                    nanos,
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dur(text: &str) -> (i32, i32, i64) {
        match duration_from_text(text).unwrap() {
            Value::Duration {
                months,
                days,
                seconds,
                nanos: 0,
            } => (months, days, seconds),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn text_forms_and_fractions() {
        assert_eq!(dur("P14DT16H12M"), (0, 14, 58320));
        assert_eq!(dur("P5M1.5D"), (5, 1, 43200));
        assert_eq!(dur("PT0.75M"), (0, 0, 45));
        assert_eq!(dur("P2.5W"), (0, 17, 43200));
        assert_eq!(dur("P12Y5M14DT16H12M70S"), (149, 14, 58390));
        assert_eq!(dur("P2012-02-02T14:37:21"), (2012 * 12 + 2, 2, 52641));
        for bad in ["14D", "P", "PT", "P1D1Y", "P1.5S", "PXD", "P1H"] {
            assert!(duration_from_text(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sub_second_components_keep_every_nanosecond() {
        let build = |text: &str| match duration_from_text(text).unwrap() {
            Value::Duration { seconds, nanos, .. } => (seconds, nanos),
            other => panic!("{other:?}"),
        };
        assert_eq!(build("PT0.000000001S"), (0, 1));
        assert_eq!(build("PT1.5S"), (1, 500_000_000));
        assert_eq!(build("PT-0.5S"), (-1, 500_000_000));
        assert_eq!(build("PT0.1M"), (6, 0));
    }

    #[test]
    fn iso_rendering() {
        assert_eq!(
            format_duration_iso(149, 14, 58390, 0),
            "P12Y5M14DT16H13M10S"
        );
        assert_eq!(format_duration_iso(0, 0, -79200, 0), "PT-22H");
        assert_eq!(format_duration_iso(0, 0, 0, 0), "PT0S");
        assert_eq!(format_duration_iso(0, 0, 91, 0), "PT1M31S");
        assert_eq!(format_duration_iso(0, 0, 0, 1), "PT0.000000001S");
        assert_eq!(format_duration_iso(0, 0, 1, 500_000_000), "PT1.5S");
        // Normalised -0.5 s is (-1 s, +500_000_000 ns).
        assert_eq!(format_duration_iso(0, 0, -1, 500_000_000), "PT-0.5S");
        assert_eq!(format_duration_iso(0, 0, -91, 250_000_000), "PT-1M-30.75S");
        assert_eq!(
            format_duration_iso(1, 2, 3723, 40_000_000),
            "P1M2DT1H2M3.04S"
        );
    }
}
