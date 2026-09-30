//! The validity-interval evaluator: how a query instant is read and how it is
//! compared with an element's `from`/`to` bounds. Cypher `valid_at` /
//! `valid_during` and the fluent temporal filters all answer through it, so a
//! bound stored as a date, a datetime or an ISO string gives one answer
//! whichever surface asks.

use crate::datatypes::values::Value;
use crate::graph::property_types::value_type_name;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;

/// `NaiveDate::num_days_from_ce` of 1970-01-01.
const EPOCH_DAYS_FROM_CE: i64 = 719_163;

/// A point on the time line, at the grain it was written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Instant {
    Date(NaiveDate),
    /// Naive UTC: an offset the source carried has already been applied.
    Timestamp(NaiveDateTime),
}

impl Instant {
    pub(crate) fn date(self) -> NaiveDate {
        match self {
            Instant::Date(d) => d,
            Instant::Timestamp(ts) => ts.date(),
        }
    }

    /// Chronological order. Two timestamps compare exactly; when either side
    /// is a date the comparison is at date grain, so a date bound covers its
    /// whole day.
    pub(crate) fn chrono_cmp(self, other: Instant) -> Ordering {
        match (self, other) {
            (Instant::Timestamp(a), Instant::Timestamp(b)) => a.cmp(&b),
            (a, b) => a.date().cmp(&b.date()),
        }
    }
}

/// Whether the `to` bound belongs to the interval.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntervalConvention {
    /// `[from, to]`: the `to` day is the last valid day.
    #[default]
    Closed,
    /// `[from, to)`: a date `to` is the first day no longer valid; a datetime
    /// `to` is the first instant, so its day is still valid when it ends after
    /// midnight.
    HalfOpen,
}

impl IntervalConvention {
    /// The spelling a declaration takes and reports: `closed` / `half_open`.
    pub fn as_str(self) -> &'static str {
        match self {
            IntervalConvention::Closed => "closed",
            IntervalConvention::HalfOpen => "half_open",
        }
    }

    /// Read [`Self::as_str`]'s spelling back; `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "closed" => Some(IntervalConvention::Closed),
            "half_open" => Some(IntervalConvention::HalfOpen),
            _ => None,
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        *self == IntervalConvention::Closed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoundSide {
    From,
    To,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TemporalError {
    /// The query instant is not a date, a datetime, or a string that
    /// `date()` / `datetime()` can read.
    Instant { found: &'static str, shown: String },
    /// A stored bound is not NULL, a date, a datetime or a readable ISO
    /// string.
    Bound {
        side: BoundSide,
        found: &'static str,
        shown: String,
    },
}

impl fmt::Display for TemporalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemporalError::Instant { found, shown } => {
                write!(f, "{shown} ({found}) is not a date or datetime")
            }
            TemporalError::Bound { side, found, shown } => {
                let side = match side {
                    BoundSide::From => "from",
                    BoundSide::To => "to",
                };
                write!(
                    f,
                    "the {side} bound {shown} ({found}) is not a date, a datetime or an ISO date string"
                )
            }
        }
    }
}

pub(crate) fn shown(value: &Value) -> String {
    match value {
        Value::String(s) => format!("'{s}'"),
        other => crate::graph::core::value_operations::format_value_compact(other),
    }
}

/// Read a string as `date()` reads it when it has no time part, and as
/// `datetime()` reads it otherwise.
fn parse_instant_str(text: &str) -> Option<Instant> {
    if let Ok((date, _)) = crate::graph::features::timeseries::parse_date_query(text) {
        return Some(Instant::Date(date));
    }
    crate::graph::languages::cypher::executor::scalar_functions::parse_datetime_utc(text)
        .map(Instant::Timestamp)
}

/// The query instant. A date or datetime value is taken as is; a string is
/// read by the `date()` parser when it has no time part (`'2009'` is
/// 2009-01-01) and by the `datetime()` parser otherwise (an offset is applied,
/// normalised to UTC). Anything else, NULL included, is an error.
pub(crate) fn parse_instant(value: &Value) -> Result<Instant, TemporalError> {
    let parsed = match value {
        Value::DateTime(d) => Some(Instant::Date(*d)),
        Value::Timestamp(ts) => Some(Instant::Timestamp(*ts)),
        Value::String(s) => parse_instant_str(s),
        _ => None,
    };
    parsed.ok_or_else(|| TemporalError::Instant {
        found: value_type_name(value),
        shown: shown(value),
    })
}

/// A stored bound: `None` for NULL (open), otherwise the instant it holds.
fn parse_bound(value: &Value, side: BoundSide) -> Result<Option<Instant>, TemporalError> {
    let parsed = match value {
        Value::Null => return Ok(None),
        Value::DateTime(d) => Some(Instant::Date(*d)),
        Value::Timestamp(ts) => Some(Instant::Timestamp(*ts)),
        Value::String(s) => parse_instant_str(s),
        _ => None,
    };
    match parsed {
        Some(instant) => Ok(Some(instant)),
        None => Err(TemporalError::Bound {
            side,
            found: value_type_name(value),
            shown: shown(value),
        }),
    }
}

/// Both stored bounds, `from` first; the first unreadable one is the error.
pub(crate) fn parse_bounds(
    from: &Value,
    to: &Value,
) -> Result<(Option<Instant>, Option<Instant>), TemporalError> {
    Ok((
        parse_bound(from, BoundSide::From)?,
        parse_bound(to, BoundSide::To)?,
    ))
}

/// `from <= t` and `t <= to` (`t < to` when half-open, as [`end_admits`]
/// reads it). NULL bounds are open. An empty interval (see [`non_empty`])
/// contains no instant, even one on its own day at date grain.
/// Both bounds are read before either is compared, so a bad bound errors
/// whatever the instant.
#[inline]
pub(crate) fn interval_contains(
    from: &Value,
    to: &Value,
    instant: Instant,
    convention: IntervalConvention,
) -> Result<bool, TemporalError> {
    // Date (or NULL) bounds compare at date grain whatever the instant's
    // grain, so the common stored shape skips building instants.
    if let (Some(from), Some(to)) = (date_bound(from), date_bound(to)) {
        let t = instant.date();
        let end_admits = |end: NaiveDate, day: NaiveDate| match convention {
            IntervalConvention::Closed => end >= day,
            IntervalConvention::HalfOpen => end > day,
        };
        let non_empty = match (from, to) {
            (Some(from), Some(to)) => end_admits(to, from),
            _ => true,
        };
        return Ok(non_empty
            && from.is_none_or(|from| from <= t)
            && to.is_none_or(|to| end_admits(to, t)));
    }
    contains_parsed(from, to, instant, convention)
}

/// [`interval_contains`] over parsed instants: every bound shape.
fn contains_parsed(
    from: &Value,
    to: &Value,
    instant: Instant,
    convention: IntervalConvention,
) -> Result<bool, TemporalError> {
    let (from, to) = parse_bounds(from, to)?;
    Ok(non_empty(from, to, convention)
        && starts_by(from, instant)
        && ends_after(to, instant, convention))
}

/// A NULL (`Some(None)`) or date (`Some(Some(day))`) bound; `None` for any
/// other value, which takes the general path.
#[inline]
fn date_bound(value: &Value) -> Option<Option<NaiveDate>> {
    match value {
        Value::Null => Some(None),
        Value::DateTime(day) => Some(Some(*day)),
        _ => None,
    }
}

/// Whether the interval holds any instant: false when its end does not
/// admit its own start — `from == to` under half-open, which a declaration
/// and every write accept (counted and warned about), or inverted, which they
/// refuse and only an unjudged writer (a fluent `update()`, an older
/// version's file) can leave. Checked apart from the instant, because at
/// date grain an inverted pair of timestamps on one day (`[08:00, 00:00]`)
/// would otherwise read as covering that day.
fn non_empty(from: Option<Instant>, to: Option<Instant>, convention: IntervalConvention) -> bool {
    match (from, to) {
        (Some(from), Some(to)) => end_admits(to, from, convention),
        _ => true,
    }
}

/// Whether the element's interval shares an instant with the closed query
/// range `[a, b]`. An empty interval (see [`non_empty`]) shares none, as
/// [`interval_contains`] finds it valid on no date.
pub(crate) fn interval_overlaps(
    from: &Value,
    to: &Value,
    a: Instant,
    b: Instant,
    convention: IntervalConvention,
) -> Result<bool, TemporalError> {
    let (from, to) = parse_bounds(from, to)?;
    Ok(non_empty(from, to, convention) && starts_by(from, b) && ends_after(to, a, convention))
}

fn starts_by(from: Option<Instant>, t: Instant) -> bool {
    from.is_none_or(|f| f.chrono_cmp(t) != Ordering::Greater)
}

fn ends_after(to: Option<Instant>, t: Instant, convention: IntervalConvention) -> bool {
    to.is_none_or(|end| end_admits(end, t, convention))
}

/// Whether the end bound `end` still admits `t`: `t <= end` closed, `t < end`
/// half-open, at [`Instant::chrono_cmp`]'s grain. One exception: half-open, a
/// timestamp end against a date `t` is compared with `t`'s midnight exactly,
/// so `[.., 2009-06-30T20:00)` holds part of 06-30 and is valid on it, while
/// `[.., 2009-06-30T00:00)` holds none of it.
pub(crate) fn end_admits(end: Instant, t: Instant, convention: IntervalConvention) -> bool {
    match (convention, end, t) {
        (IntervalConvention::Closed, _, _) => end.chrono_cmp(t) != Ordering::Less,
        (IntervalConvention::HalfOpen, Instant::Timestamp(end), Instant::Date(day)) => {
            end > day.and_time(NaiveTime::MIN)
        }
        (IntervalConvention::HalfOpen, _, _) => end.chrono_cmp(t) == Ordering::Greater,
    }
}

/// Microseconds in a day.
pub(crate) const DAY_US: i64 = 86_400_000_000;

/// A query instant read against timestamp bounds held as epoch microseconds.
#[derive(Clone, Copy, Debug)]
pub(crate) enum MicrosProbe {
    /// A timestamp, compared with a bound exactly.
    Timestamp(i64),
    /// A date, compared with a bound at date grain (`day`: days since the
    /// epoch), except a half-open end, which is compared with the day's
    /// midnight exactly.
    Date { day: i64 },
}

impl MicrosProbe {
    /// The probe for `t`; `None` for a timestamp the microsecond encoding
    /// cannot hold exactly, which takes the general path.
    pub(crate) fn of(t: Instant) -> Option<Self> {
        match t {
            Instant::Date(date) => {
                let days_from_ce = i64::from(chrono::Datelike::num_days_from_ce(&date));
                Some(MicrosProbe::Date {
                    day: days_from_ce - EPOCH_DAYS_FROM_CE,
                })
            }
            Instant::Timestamp(ts) => {
                crate::graph::storage::column_store::exact_micros(ts).map(MicrosProbe::Timestamp)
            }
        }
    }

    /// [`Instant::chrono_cmp`] of the bound `bound` against this instant.
    #[inline]
    fn cmp_bound(self, bound: i64) -> Ordering {
        match self {
            MicrosProbe::Timestamp(us) => bound.cmp(&us),
            MicrosProbe::Date { day } => bound.div_euclid(DAY_US).cmp(&day),
        }
    }

    /// [`end_admits`] for an `end` bound.
    #[inline]
    fn end_admits(self, end: i64, convention: IntervalConvention) -> bool {
        match (convention, self) {
            (IntervalConvention::Closed, _) => self.cmp_bound(end) != Ordering::Less,
            (IntervalConvention::HalfOpen, MicrosProbe::Date { day }) => end > day * DAY_US,
            (IntervalConvention::HalfOpen, MicrosProbe::Timestamp(us)) => end > us,
        }
    }
}

/// [`interval_overlaps`] over timestamp bounds in epoch microseconds (`None`
/// is an open bound); [`interval_contains`] is the case `a == b`. Answers as
/// [`non_empty`], [`starts_by`] and [`ends_after`] do for the same bounds.
#[inline]
pub(crate) fn micros_interval_overlaps(
    from: Option<i64>,
    to: Option<i64>,
    a: MicrosProbe,
    b: MicrosProbe,
    convention: IntervalConvention,
) -> bool {
    let non_empty = match (from, to) {
        (Some(from), Some(to)) => MicrosProbe::Timestamp(from).end_admits(to, convention),
        _ => true,
    };
    non_empty
        && from.is_none_or(|from| b.cmp_bound(from) != Ordering::Greater)
        && to.is_none_or(|to| a.end_admits(to, convention))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_date_fast_path_agrees_with_the_parsed_path() {
        let days = ["2009-06-29", "2009-06-30", "2009-07-01"];
        let mut bounds: Vec<Value> = days.iter().map(|day| d(day)).collect();
        bounds.push(Value::Null);
        let instants = [
            d("2009-06-30"),
            ts("2009-06-30T00:00"),
            ts("2009-06-30T12:00"),
        ];
        for from in &bounds {
            for to in &bounds {
                for t in &instants {
                    for c in [CLOSED, HALF_OPEN] {
                        assert_eq!(
                            interval_contains(from, to, at(t.clone()), c),
                            contains_parsed(from, to, at(t.clone()), c),
                            "{from:?} {to:?} {t:?} {c:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn an_empty_interval_overlaps_no_range() {
        let (a, b) = (
            Instant::Date(NaiveDate::from_ymd_opt(1900, 1, 1).unwrap()),
            Instant::Date(NaiveDate::from_ymd_opt(2100, 1, 1).unwrap()),
        );
        let closed = IntervalConvention::Closed;
        let half_open = IntervalConvention::HalfOpen;
        let inverted = (d("2005-01-01"), d("1990-01-01"));
        assert!(!interval_overlaps(&inverted.0, &inverted.1, a, b, closed).unwrap());
        assert!(!interval_overlaps(&inverted.0, &inverted.1, a, b, half_open).unwrap());
        let one_day = (d("2005-01-01"), d("2005-01-01"));
        assert!(interval_overlaps(&one_day.0, &one_day.1, a, b, closed).unwrap());
        assert!(!interval_overlaps(&one_day.0, &one_day.1, a, b, half_open).unwrap());
        // A datetime end later on the start day leaves part of that day.
        let part_day = (d("2005-01-01"), ts("2005-01-01T20:00"));
        assert!(interval_overlaps(&part_day.0, &part_day.1, a, b, half_open).unwrap());
        assert!(interval_overlaps(&Value::Null, &inverted.1, a, b, half_open).unwrap());
    }

    fn d(s: &str) -> Value {
        Value::DateTime(NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap())
    }

    fn ts(s: &str) -> Value {
        Value::Timestamp(NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").unwrap())
    }

    fn s(text: &str) -> Value {
        Value::String(text.to_string())
    }

    fn at(v: Value) -> Instant {
        parse_instant(&v).unwrap()
    }

    fn contains(from: &Value, to: &Value, t: Value, c: IntervalConvention) -> bool {
        interval_contains(from, to, at(t), c).unwrap()
    }

    const CLOSED: IntervalConvention = IntervalConvention::Closed;
    const HALF_OPEN: IntervalConvention = IntervalConvention::HalfOpen;

    #[test]
    fn an_empty_interval_on_one_day_contains_no_instant_of_that_day() {
        // At date grain both bounds equal the day; the interval is still empty.
        let (from, to) = (ts("2009-06-30T08:00"), ts("2009-06-30T00:00"));
        assert!(!contains(&from, &to, d("2009-06-30"), CLOSED));
        assert!(!contains(&from, &to, d("2009-06-30"), HALF_OPEN));
        let same = ts("2009-06-30T08:00");
        assert!(!contains(&same, &same, d("2009-06-30"), HALF_OPEN));
        assert!(contains(&same, &same, d("2009-06-30"), CLOSED));
        assert!(contains(&same, &same, ts("2009-06-30T08:00"), CLOSED));
    }

    #[test]
    fn date_only_strings_read_as_date_does() {
        let jan1 = Instant::Date(NaiveDate::from_ymd_opt(2009, 1, 1).unwrap());
        assert_eq!(parse_instant(&s("2009")), Ok(jan1));
        assert_eq!(
            parse_instant(&s("2009-06")),
            Ok(Instant::Date(NaiveDate::from_ymd_opt(2009, 6, 1).unwrap()))
        );
        assert_eq!(
            parse_instant(&s("2009-06-30")),
            Ok(Instant::Date(NaiveDate::from_ymd_opt(2009, 6, 30).unwrap()))
        );
        assert_eq!(parse_instant(&d("2009-01-01")), Ok(jan1));
    }

    #[test]
    fn a_string_offset_is_applied_not_dropped() {
        let got = parse_instant(&s("2009-06-30T01:00:00+02:00")).unwrap();
        assert_eq!(got, at(ts("2009-06-29T23:00")));
        let minutes = parse_instant(&s("2009-06-30T01:00+02:00")).unwrap();
        assert_eq!(minutes, got);
    }

    #[test]
    fn unreadable_instants_are_errors() {
        for bad in [s("garbage"), Value::Int64(2009), Value::Null, s("")] {
            assert!(
                matches!(parse_instant(&bad), Err(TemporalError::Instant { .. })),
                "{bad:?}"
            );
        }
        assert_eq!(
            parse_instant(&Value::Int64(2009)),
            Err(TemporalError::Instant {
                found: "INTEGER",
                shown: "2009".into()
            })
        );
    }

    #[test]
    fn null_bounds_are_open() {
        assert!(contains(&Value::Null, &Value::Null, s("1900"), CLOSED));
        assert!(contains(&d("2005-01-01"), &Value::Null, s("2100"), CLOSED));
        assert!(!contains(&d("2005-01-01"), &Value::Null, s("2004"), CLOSED));
    }

    #[test]
    fn every_bound_kind_answers_alike() {
        let bounds = [
            (d("2005-01-01"), d("2012-12-31")),
            (s("2005-01-01"), s("2012-12-31")),
            (ts("2005-01-01T00:00"), ts("2012-12-31T12:00")),
            (s("2005"), s("2012-12-31T12:00:00")),
        ];
        for (from, to) in &bounds {
            assert!(contains(from, to, s("2009"), CLOSED), "{from:?}");
            assert!(contains(from, to, s("2005"), CLOSED), "{from:?}");
            assert!(!contains(from, to, s("2004-12-31"), CLOSED), "{from:?}");
            assert!(!contains(from, to, s("2013"), CLOSED), "{from:?}");
        }
    }

    #[test]
    fn timestamp_bound_is_exact_against_a_timestamp_and_daily_against_a_date() {
        let from = d("2000-01-01");
        let to = ts("2009-06-29T23:30");
        assert!(contains(&from, &to, ts("2009-06-29T23:00"), CLOSED));
        assert!(!contains(
            &from,
            &ts("2009-06-29T22:30"),
            ts("2009-06-29T23:00"),
            CLOSED
        ));
        // A date instant compares at date grain: 22:30 on the 29th still
        // covers the 29th.
        assert!(contains(
            &from,
            &ts("2009-06-29T22:30"),
            d("2009-06-29"),
            CLOSED
        ));
        assert!(!contains(
            &from,
            &ts("2009-06-29T22:30"),
            d("2009-06-30"),
            CLOSED
        ));
    }

    #[test]
    fn date_bound_covers_its_whole_day_against_a_timestamp() {
        let from = d("2000-01-01");
        let to = d("2009-06-29");
        assert!(contains(&from, &to, ts("2009-06-29T23:00"), CLOSED));
        assert!(!contains(&from, &to, ts("2009-06-30T00:00"), CLOSED));
        assert!(contains(
            &d("2009-06-29"),
            &Value::Null,
            ts("2009-06-29T00:00"),
            CLOSED
        ));
    }

    #[test]
    fn closed_and_half_open_differ_only_on_the_to_day() {
        let from = d("2009-01-01");
        let to = d("2009-06-30");
        assert!(contains(&from, &to, s("2009-06-30"), CLOSED));
        assert!(!contains(&from, &to, s("2009-06-30"), HALF_OPEN));
        assert!(contains(&from, &to, s("2009-06-29"), HALF_OPEN));
        // The from day belongs to the interval under both conventions.
        assert!(contains(&from, &to, s("2009-01-01"), HALF_OPEN));
        assert!(!contains(&from, &to, s("2008-12-31"), HALF_OPEN));
        let ts_to = ts("2009-06-30T12:00");
        assert!(contains(&from, &ts_to, ts("2009-06-30T11:59"), HALF_OPEN));
        assert!(!contains(&from, &ts_to, ts("2009-06-30T12:00"), HALF_OPEN));
        assert!(contains(&from, &ts_to, ts("2009-06-30T12:00"), CLOSED));
    }

    #[test]
    fn a_half_open_timestamp_end_is_exact_against_a_dates_midnight() {
        // The interval exists only on 06-30, so it is valid on 06-30.
        let from = ts("2009-06-30T08:00");
        let to = ts("2009-06-30T20:00");
        assert!(contains(&from, &to, d("2009-06-30"), HALF_OPEN));
        assert!(!contains(&from, &to, d("2009-07-01"), HALF_OPEN));
        assert!(!contains(&from, &to, d("2009-06-29"), HALF_OPEN));
        // A date from with a mid-day end is valid on its day; an end at
        // midnight holds none of that day.
        assert!(contains(
            &d("2009-06-01"),
            &ts("2009-06-30T12:00"),
            d("2009-06-30"),
            HALF_OPEN
        ));
        assert!(!contains(
            &d("2009-06-01"),
            &ts("2009-06-30T00:00"),
            d("2009-06-30"),
            HALF_OPEN
        ));
        // A string timestamp end reads the same.
        assert!(contains(
            &from,
            &s("2009-06-30T20:00:00"),
            d("2009-06-30"),
            HALF_OPEN
        ));
        // Every other pairing keeps the date-grain rule.
        assert!(!contains(
            &from,
            &d("2009-06-30"),
            d("2009-06-30"),
            HALF_OPEN
        ));
        assert!(contains(&from, &to, d("2009-06-30"), CLOSED));
    }

    #[test]
    fn a_half_open_timestamp_end_overlaps_a_range_starting_on_its_day() {
        let overlaps = |to: Value, a: &str| {
            interval_overlaps(
                &d("2009-06-01"),
                &to,
                at(s(a)),
                at(s("2009-07-10")),
                HALF_OPEN,
            )
            .unwrap()
        };
        assert!(overlaps(ts("2009-06-30T12:00"), "2009-06-30"));
        assert!(!overlaps(ts("2009-06-30T00:00"), "2009-06-30"));
        assert!(!overlaps(ts("2009-06-30T12:00"), "2009-07-01"));
    }

    #[test]
    fn overlap_is_closed_on_the_query_range() {
        let from = d("2009-01-01");
        let to = d("2009-06-30");
        let overlaps =
            |a: &str, b: &str, c| interval_overlaps(&from, &to, at(s(a)), at(s(b)), c).unwrap();
        assert!(overlaps("2000", "2009", CLOSED));
        assert!(!overlaps("2000", "2008-12-31", CLOSED));
        assert!(overlaps("2009-06-30", "2010", CLOSED));
        assert!(!overlaps("2009-06-30", "2010", HALF_OPEN));
        assert!(!overlaps("2009-07-01", "2010", CLOSED));
        assert!(interval_overlaps(
            &Value::Null,
            &Value::Null,
            at(s("1900")),
            at(s("1901")),
            CLOSED
        )
        .unwrap());
    }

    #[test]
    fn unreadable_bounds_are_errors_whatever_the_instant() {
        let err = interval_contains(&s("someday"), &d("2012-12-31"), at(s("2009")), CLOSED);
        assert_eq!(
            err,
            Err(TemporalError::Bound {
                side: BoundSide::From,
                found: "STRING",
                shown: "'someday'".into()
            })
        );
        // The from bound alone already excludes the instant; the bad to bound
        // is still reported.
        let err = interval_contains(&d("2010-01-01"), &Value::Int64(2012), at(s("2009")), CLOSED);
        assert!(matches!(
            err,
            Err(TemporalError::Bound {
                side: BoundSide::To,
                found: "INTEGER",
                ..
            })
        ));
    }

    /// The integer comparison over epoch microseconds answers exactly as the
    /// parsed-instant path, for every mix of bound shape (open, timestamp),
    /// instant grain (date, timestamp), convention and range, around midnight
    /// and either side of the epoch.
    #[test]
    fn the_micros_path_agrees_with_the_parsed_path() {
        let stamps: Vec<NaiveDateTime> = [
            "1900-01-01T12:00:00",
            "1969-12-31T23:59:59",
            "1970-01-01T00:00:00",
            "2009-06-29T23:59:59",
            "2009-06-30T00:00:00",
            "2009-06-30T08:00:00",
            "2009-06-30T20:00:00",
            "2009-07-01T00:00:00",
        ]
        .iter()
        .map(|t| NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S").unwrap())
        .collect();
        let micros = |t: NaiveDateTime| crate::graph::storage::column_store::exact_micros(t);
        let bound = |t: Option<NaiveDateTime>| t.map_or(Value::Null, Value::Timestamp);
        let mut bounds: Vec<Option<NaiveDateTime>> = vec![None];
        bounds.extend(stamps.iter().copied().map(Some));
        let mut instants: Vec<Instant> = stamps.iter().copied().map(Instant::Timestamp).collect();
        instants.extend(stamps.iter().map(|t| Instant::Date(t.date())));
        for from in &bounds {
            for to in &bounds {
                for &a in &instants {
                    for &b in &instants {
                        for c in [CLOSED, HALF_OPEN] {
                            let want =
                                interval_overlaps(&bound(*from), &bound(*to), a, b, c).unwrap();
                            let got = micros_interval_overlaps(
                                from.and_then(micros),
                                to.and_then(micros),
                                MicrosProbe::of(a).unwrap(),
                                MicrosProbe::of(b).unwrap(),
                                c,
                            );
                            assert_eq!(got, want, "{from:?} {to:?} [{a:?}, {b:?}] {c:?}");
                        }
                    }
                    let want = interval_contains(&bound(*from), &bound(*to), a, HALF_OPEN).unwrap();
                    let p = MicrosProbe::of(a).unwrap();
                    let got = micros_interval_overlaps(
                        from.and_then(micros),
                        to.and_then(micros),
                        p,
                        p,
                        HALF_OPEN,
                    );
                    assert_eq!(got, want, "{from:?} {to:?} at {a:?}");
                }
            }
        }
    }

    #[test]
    fn a_timestamp_instant_the_encoding_cannot_hold_has_no_micros_probe() {
        let fine = NaiveDate::from_ymd_opt(2009, 6, 30)
            .unwrap()
            .and_hms_nano_opt(12, 0, 0, 123)
            .unwrap();
        assert!(MicrosProbe::of(Instant::Timestamp(fine)).is_none());
        assert!(MicrosProbe::of(Instant::Date(fine.date())).is_some());
    }
}
