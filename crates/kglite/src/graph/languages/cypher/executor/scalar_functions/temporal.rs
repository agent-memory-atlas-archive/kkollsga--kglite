//! Cypher scalar functions — temporal category. Split out of the monolithic
//! `evaluate_scalar_function` dispatcher. Routed from
//! `super::evaluate_scalar_function`; returns `Ok(None)` when `name` is not
//! one of this category's functions so the dispatcher tries the next.
use super::super::*;
use super::shared::*;
use super::temporal_duration;
use super::temporal_iso::parse_date_text;
use super::temporal_map::{temporal_from_map, MapTarget};
use super::temporal_truncate;
use crate::datatypes::values::Value;
use crate::graph::core::value_operations::checked_shift_months;

impl<'a> CypherExecutor<'a> {
    pub(super) fn eval_temporal_fn(
        &self,
        name: &str,
        args: &[Expression],
        row: &ResultRow,
    ) -> Result<Option<Value>, String> {
        let result: Result<Value, String> = match name {
            "date" => self.eval_date(args, row),
            "datetime" => self.eval_datetime(args, row),
            "date_diff" | "datediff" => {
                if args.len() != 2 {
                    return Err("date_diff() requires 2 date arguments".into());
                }
                let a = self.evaluate_expression(&args[0], row)?;
                let b = self.evaluate_expression(&args[1], row)?;
                if matches!(a, Value::Null) || matches!(b, Value::Null) {
                    return Ok(Some(Value::Null));
                }
                // Accepts date() and datetime() operands (and a mix).
                match (coerce_naive_datetime(&a), coerce_naive_datetime(&b)) {
                    (Some(d1), Some(d2)) => Ok(Value::Int64((d1 - d2).num_days())),
                    _ => Err("date_diff() arguments must be dates".into()),
                }
            }
            // 0.9.0 §3 / Cluster 2 — proper Value::Duration variant.
            // Calendar units (months/years) and clock units
            // (days/hours/minutes/seconds) are kept separate in the
            // value, so `duration({months: 1, days: 5}).months` returns
            // 1, not 35. Sub-day precision is wired in `seconds` —
            // DateTime + Duration discards the seconds component for
            // now because Value::DateTime is still NaiveDate (Cluster
            // 1, deferred).
            "duration" => self.eval_duration(args, row),
            "duration.between" => {
                if args.len() != 2 {
                    return Err("duration.between() requires 2 datetime arguments".into());
                }
                let a = self.evaluate_expression(&args[0], row)?;
                let b = self.evaluate_expression(&args[1], row)?;
                if matches!(a, Value::Null) || matches!(b, Value::Null) {
                    return Ok(Some(Value::Null));
                }
                // Accepts date() and datetime() operands (and a mix). Whole
                // days go in `days`; any remaining sub-day delta (when a
                // Timestamp is involved) is carried in `seconds` and `nanos`.
                duration_between_values(&a, &b)
            }
            // Temporal arithmetic (2026-05-25 broad-scan lift).
            // Real use case: "events scheduled in the next N days":
            //   MATCH (e:Event) WHERE e.date <= add_days(date(), 30)
            // Date math via chrono — NaiveDate handles month/year
            // arithmetic correctly (Feb 28 + 1 year = Feb 28; Jan 31
            // + 1 month = Feb 28/29 depending on leap year).
            "add_days" => {
                if args.len() != 2 {
                    return Err("add_days() requires 2 args: add_days(date, n_days)".into());
                }
                let d = self.evaluate_expression(&args[0], row)?;
                let n = self.evaluate_expression(&args[1], row)?;
                match (&d, &n) {
                    (Value::DateTime(date), Value::Int64(n)) => {
                        match chrono::TimeDelta::try_days(*n)
                            .and_then(|delta| date.checked_add_signed(delta))
                        {
                            Some(out) => Ok(Value::DateTime(out)),
                            None => Ok(Value::Null),
                        }
                    }
                    (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                    _ => Err("add_days() expects (date, integer)".into()),
                }
            }
            "add_months" => {
                if args.len() != 2 {
                    return Err("add_months() requires 2 args: add_months(date, n_months)".into());
                }
                let d = self.evaluate_expression(&args[0], row)?;
                let n = self.evaluate_expression(&args[1], row)?;
                match (&d, &n) {
                    (Value::DateTime(date), Value::Int64(n)) => {
                        let result = checked_shift_months(*date, *n);
                        match result {
                            Some(out) => Ok(Value::DateTime(out)),
                            None => Ok(Value::Null),
                        }
                    }
                    (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                    _ => Err("add_months() expects (date, integer)".into()),
                }
            }
            "add_years" => {
                if args.len() != 2 {
                    return Err("add_years() requires 2 args: add_years(date, n_years)".into());
                }
                let d = self.evaluate_expression(&args[0], row)?;
                let n = self.evaluate_expression(&args[1], row)?;
                match (&d, &n) {
                    (Value::DateTime(date), Value::Int64(n)) => {
                        // 12 months per year — chrono's Months handles
                        // leap-year edge case (Feb 29 + 1 year → Feb 28).
                        let result = n
                            .checked_mul(12)
                            .and_then(|months_delta| checked_shift_months(*date, months_delta));
                        match result {
                            Some(out) => Ok(Value::DateTime(out)),
                            None => Ok(Value::Null),
                        }
                    }
                    (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                    _ => Err("add_years() expects (date, integer)".into()),
                }
            }
            // date_truncate(date, 'unit') — round down to the start of
            // a calendar period. Real use case: group analytics by
            // week/month: `RETURN date_truncate(e.ts, 'month'), count(e)`.
            "date_truncate" => {
                use chrono::{Datelike, NaiveDate};
                if args.len() != 2 {
                    return Err(
                        "date_truncate() requires 2 args: date_truncate(date, 'year'|'month'|'week'|'day')".into()
                    );
                }
                let d = self.evaluate_expression(&args[0], row)?;
                let unit = self.evaluate_expression(&args[1], row)?;
                let (date, unit_str) = match (&d, &unit) {
                    (Value::DateTime(date), Value::String(u)) => (*date, u.as_str()),
                    (Value::Null, _) | (_, Value::Null) => return Ok(Some(Value::Null)),
                    _ => return Err("date_truncate() expects (date, string)".into()),
                };
                let truncated = match unit_str {
                    "year" | "years" => NaiveDate::from_ymd_opt(date.year(), 1, 1),
                    "month" | "months" => NaiveDate::from_ymd_opt(date.year(), date.month(), 1),
                    "week" | "weeks" => {
                        // ISO week starts Monday. Subtract weekday-1 days.
                        let dow = date.weekday().num_days_from_monday() as i64;
                        date.checked_sub_signed(chrono::Duration::days(dow))
                    }
                    "day" | "days" => Some(date),
                    other => {
                        return Err(format!(
                            "date_truncate() unit must be year/month/week/day, got '{other}'"
                        ));
                    }
                };
                Ok(truncated.map(Value::DateTime).unwrap_or(Value::Null))
            }
            n if temporal_truncate::is_namespaced(n) => self.eval_namespaced_temporal(n, args, row),
            "localdatetime" => self.eval_localdatetime(args, row),
            "localtime" => self.eval_localtime(args, row),
            "time" => self.eval_time(args, row),
            _ => return Ok(None),
        };
        result.map(Some)
    }
}

impl CypherExecutor<'_> {
    /// `duration({...})`: calendar units (months/years) and clock units
    /// (days/hours/minutes/seconds) are kept apart, so
    /// `duration({months: 1, days: 5}).months` is 1, not 35.
    fn eval_duration(&self, args: &[Expression], row: &ResultRow) -> Result<Value, String> {
        if args.len() != 1 {
            return Err(
                "duration() requires 1 argument: duration({days: N}) or duration('P1D')".into(),
            );
        }
        match self.evaluate_expression(&args[0], row)? {
            Value::Map(map) => temporal_duration::duration_from_map(&map),
            Value::String(text) => temporal_duration::duration_from_text(&text),
            duration @ Value::Duration { .. } => Ok(duration),
            Value::Null => Ok(Value::Null),
            other => Err(format!(
                "duration() requires a map or an ISO-8601 string, got {}",
                other.type_name()
            )),
        }
    }

    /// `date()` (today), `date(string | map | date)`.
    fn eval_date(&self, args: &[Expression], row: &ResultRow) -> Result<Value, String> {
        // Today in UTC: a valid-time instant is naive UTC, and
        // `FOR VALID_TIME AS OF date()` resolves through here.
        if args.is_empty() {
            return Ok(Value::DateTime(chrono::Utc::now().date_naive()));
        }
        if args.len() != 1 {
            return Err("date() takes 0 or 1 argument: date() or date('2020-01-15')".into());
        }
        let val = self.evaluate_expression(&args[0], row)?;
        match val {
            Value::String(s) => {
                // An unparsable literal raises: a NULL here becomes a silent
                // open validity bound or a missing property once written.
                if let Some(date) = parse_date_text(&s) {
                    return Ok(Value::DateTime(date));
                }
                match crate::graph::features::timeseries::parse_date_query(&s) {
                    Ok((d, _)) => Ok(Value::DateTime(d)),
                    Err(_) => Err(format!(
                        "date(): cannot parse '{s}' as a date (expected YYYY, YYYY-MM, YYYY-MM-DD, YYYYMMDD, an ISO week date or an ordinal date)"
                    )),
                }
            }
            Value::DateTime(_) => Ok(val),
            Value::Timestamp(t) => Ok(Value::DateTime(t.date())),
            // The calendar date on the wall clock the value was written in.
            Value::ZonedDateTime { utc, offset_secs } => Ok(Value::DateTime(
                crate::datatypes::zoned_datetime::wall_clock(utc, offset_secs).date(),
            )),
            Value::Map(map) => temporal_from_map(MapTarget::Date, &map),
            Value::Null => Ok(Value::Null),
            _ => Err(format!(
                "date() argument must be a string, a date, a datetime or a map, got {:?}",
                val
            )),
        }
    }
}

/// `duration.between` of two temporal values: two times of day differ in
/// seconds and nanoseconds, anything else is read as a date or datetime.
fn duration_between_values(a: &Value, b: &Value) -> Result<Value, String> {
    if let Some(delta) = super::temporal_time::time_between(a, b) {
        return delta;
    }
    // Two instants differ by the time between them. With a local side the zoned
    // one is read at its own wall clock (its offset is dropped), which
    // `coerce_naive_datetime` does.
    if let (Value::ZonedDateTime { utc: start, .. }, Value::ZonedDateTime { utc: end, .. }) = (a, b)
    {
        return duration_between(*start, *end);
    }
    match (coerce_naive_datetime(a), coerce_naive_datetime(b)) {
        (Some(start), Some(end)) => duration_between(start, end),
        _ => Err("duration.between() arguments must be datetime values".into()),
    }
}

/// `duration.between` of two instants: whole days in `days`, the sub-day
/// remainder in `seconds` and `nanos`.
fn duration_between(
    start: chrono::NaiveDateTime,
    end: chrono::NaiveDateTime,
) -> Result<Value, String> {
    const DAY_NANOS: i128 = 86_400 * 1_000_000_000;
    let delta = end - start;
    let total = i128::from(delta.num_seconds()) * 1_000_000_000 + i128::from(delta.subsec_nanos());
    // `/` and `%` truncate toward zero, so days and the remainder share the
    // delta's sign, as before.
    let (seconds, nanos) = crate::graph::core::value_operations::split_clock(total % DAY_NANOS)
        .ok_or("duration.between() delta exceeds the supported range")?;
    Ok(Value::Duration {
        months: 0,
        days: i32::try_from(total / DAY_NANOS)
            .map_err(|_| "duration.between() day component exceeds the supported i32 range")?,
        seconds,
        nanos,
    })
}
