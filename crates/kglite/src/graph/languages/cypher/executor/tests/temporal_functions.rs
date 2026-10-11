//! Absolute goldens for the openCypher temporal functions on the value model
//! this engine has: a date, a zoneless datetime (`Timestamp`), a time of day
//! (`LocalTime`, `Time`), and a months/days/seconds duration. Constructor key forms,
//! ISO 8601 string spellings and canonical rendering are shared executor
//! semantics, so the differential corpus cannot see them; each expectation is
//! the value the openCypher specification gives.

use super::*;

pub(super) fn run(query: &str) -> Result<Value, String> {
    let graph = DirGraph::new();
    let parsed = parser::parse_cypher(query)
        .unwrap_or_else(|e| panic!("query failed to parse: {query}\n  error: {e}"));
    let no_params = HashMap::new();
    let result = CypherExecutor::with_params(&graph, &no_params, None).execute(&parsed)?;
    assert_eq!(result.rows.len(), 1, "expected one row from: {query}");
    Ok(result.rows[0][0].clone())
}

pub(super) fn cell(query: &str) -> Value {
    run(query).unwrap_or_else(|e| panic!("query failed: {query}\n  error: {e}"))
}

fn date(y: i32, m: u32, d: u32) -> Value {
    Value::DateTime(chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap())
}

fn text(s: &str) -> Value {
    Value::String(s.to_string())
}

fn local_time(clock: &str) -> Value {
    let time = crate::datatypes::time_of_day::parse_clock(clock).unwrap();
    Value::LocalTime(crate::datatypes::time_of_day::nanos_of(time))
}

fn zoned_time(clock: &str, offset_secs: i32) -> Value {
    let time = crate::datatypes::time_of_day::parse_clock(clock).unwrap();
    Value::Time {
        nanos: crate::datatypes::time_of_day::nanos_of(time),
        offset_secs,
    }
}

fn local_datetime(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32, nanos: u32) -> Value {
    Value::Timestamp(
        chrono::NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_nano_opt(h, mi, s, nanos)
            .unwrap(),
    )
}

#[test]
fn date_map_forms() {
    for (map, expected) in [
        ("{year: 1816, week: 1}", date(1816, 1, 1)),
        ("{year: 1817, week: 1}", date(1816, 12, 30)),
        ("{year: 1818, week: 53}", date(1818, 12, 28)),
        ("{dayOfWeek: 2, year: 1817, week: 1}", date(1816, 12, 31)),
        (
            "{date: date('1816-12-30'), week: 2, dayOfWeek: 3}",
            date(1817, 1, 8),
        ),
        ("{date: date('1816-12-31'), week: 2}", date(1817, 1, 7)),
        (
            "{date: date('1816-12-31'), year: 1817, week: 2}",
            date(1817, 1, 7),
        ),
        ("{year: 1984, week: 10, dayOfWeek: 3}", date(1984, 3, 7)),
        ("{year: 1984, ordinalDay: 202}", date(1984, 7, 20)),
        (
            "{year: 1984, quarter: 3, dayOfQuarter: 45}",
            date(1984, 8, 14),
        ),
        ("{year: 1984, quarter: 3}", date(1984, 7, 1)),
        ("{date: date('1984-11-11'), year: 28}", date(28, 11, 11)),
        ("{date: date('1984-11-11'), week: 1}", date(1984, 1, 8)),
        (
            "{date: date('1984-11-11'), ordinalDay: 28}",
            date(1984, 1, 28),
        ),
        ("{date: date('1984-11-11'), quarter: 3}", date(1984, 8, 11)),
        (
            "{date: localdatetime('1984-11-11T12:31:14.645876123'), day: 28}",
            date(1984, 11, 28),
        ),
    ] {
        assert_eq!(cell(&format!("RETURN date({map})")), expected, "{map}");
    }
}

#[test]
fn date_string_spellings() {
    for (input, expected) in [
        ("20150721", date(2015, 7, 21)),
        ("201507", date(2015, 7, 1)),
        ("2015-W30-2", date(2015, 7, 21)),
        ("2015W302", date(2015, 7, 21)),
        ("2015-W30", date(2015, 7, 20)),
        ("2015-202", date(2015, 7, 21)),
        ("2015202", date(2015, 7, 21)),
        ("2015", date(2015, 1, 1)),
    ] {
        assert_eq!(
            cell(&format!("RETURN date('{input}')")),
            expected,
            "{input}"
        );
    }
    assert!(run("RETURN date('2015-W54')").is_err());
    assert!(run("RETURN date('2015-367')").is_err());
}

#[test]
fn local_time_maps_and_strings_render_canonically() {
    for (query, expected) in [
        (
            "localtime({hour: 12, minute: 31, second: 14, nanosecond: 789, millisecond: 123, microsecond: 456})",
            "12:31:14.123456789",
        ),
        ("localtime({hour: 12, minute: 31, second: 14, nanosecond: 3})", "12:31:14.000000003"),
        ("localtime({hour: 12, minute: 31, second: 14, microsecond: 645876})", "12:31:14.645876"),
        ("localtime({hour: 12, minute: 31, second: 14, millisecond: 645})", "12:31:14.645"),
        ("localtime({hour: 12, minute: 31})", "12:31"),
        ("localtime({hour: 12})", "12:00"),
        ("localtime('214032.142')", "21:40:32.142"),
        ("localtime('2140')", "21:40"),
        ("localtime('21')", "21:00"),
        (
            "localtime({time: localtime({hour: 12, minute: 31, second: 14, nanosecond: 645876123}), second: 42})",
            "12:31:42.645876123",
        ),
        (
            "localtime(localdatetime({year: 1984, week: 10, dayOfWeek: 3, hour: 12, minute: 31, second: 14, millisecond: 645}))",
            "12:31:14.645",
        ),
    ] {
        assert_eq!(
            cell(&format!("RETURN {query}")),
            local_time(expected),
            "{query}"
        );
        assert_eq!(
            cell(&format!("RETURN toString({query})")),
            text(expected),
            "{query}"
        );
    }
}

#[test]
fn local_datetime_maps_and_strings() {
    for (query, expected) in [
        (
            "localdatetime({year: 1984, month: 10, day: 11, hour: 12, minute: 31, second: 14, nanosecond: 645876123})",
            local_datetime(1984, 10, 11, 12, 31, 14, 645_876_123),
        ),
        ("localdatetime({year: 1984})", local_datetime(1984, 1, 1, 0, 0, 0, 0)),
        (
            "localdatetime({year: 1984, week: 10, dayOfWeek: 3, hour: 12, millisecond: 645})",
            local_datetime(1984, 3, 7, 12, 0, 0, 645_000_000),
        ),
        (
            "localdatetime({year: 1984, quarter: 3, dayOfQuarter: 45, hour: 12})",
            local_datetime(1984, 8, 14, 12, 0, 0, 0),
        ),
        (
            "localdatetime({date: date('1984-10-11'), time: localtime('12:31:14.645876123'), day: 28, second: 42})",
            local_datetime(1984, 10, 28, 12, 31, 42, 645_876_123),
        ),
        (
            "localdatetime({datetime: localdatetime('1984-03-07T12:31:14.645'), day: 28, second: 42})",
            local_datetime(1984, 3, 28, 12, 31, 42, 645_000_000),
        ),
        ("localdatetime('2015-W30-2T214032.142')", local_datetime(2015, 7, 21, 21, 40, 32, 142_000_000)),
        ("localdatetime('2015-202T21:40:32')", local_datetime(2015, 7, 21, 21, 40, 32, 0)),
        ("localdatetime('2015T214032')", local_datetime(2015, 1, 1, 21, 40, 32, 0)),
        ("localdatetime('20150721T21:40')", local_datetime(2015, 7, 21, 21, 40, 0, 0)),
        ("localdatetime('2015202T21')", local_datetime(2015, 7, 21, 21, 0, 0, 0)),
        (
            "localdatetime(localdatetime('1984-03-07T12:31:14.645'))",
            local_datetime(1984, 3, 7, 12, 31, 14, 645_000_000),
        ),
    ] {
        assert_eq!(cell(&format!("RETURN {query}")), expected, "{query}");
    }
}

#[test]
fn temporal_map_refusals_stay_errors() {
    for query in [
        "RETURN date({year: 1984, week: 54})",
        "RETURN date({year: 1984, ordinalDay: 400})",
        "RETURN date({year: 1984, quarter: 5})",
        "RETURN date({year: 1984, quarter: 3, dayOfQuarter: 93})",
        "RETURN date({year: 1984, hour: 1})",
        "RETURN localtime({hour: 25})",
        "RETURN localtime({year: 1984})",
        "RETURN localdatetime({hour: 1})",
        "RETURN localtime('24:61')",
    ] {
        assert!(run(query).is_err(), "{query}");
    }
    assert_eq!(cell("RETURN localtime({hour: null})"), Value::Null);
}

#[test]
fn truncate_rounds_down_then_applies_overrides() {
    let ldt = "localdatetime({year: 1984, month: 10, day: 11, hour: 12, minute: 31, second: 14, nanosecond: 645876123})";
    for (query, expected) in [
        (
            "date.truncate('millennium', date({year: 2017, month: 10, day: 11}), {day: 2})"
                .to_string(),
            date(2000, 1, 2),
        ),
        (
            format!("date.truncate('century', {ldt}, {{}})"),
            date(1900, 1, 1),
        ),
        (format!("date.truncate('decade', {ldt})"), date(1980, 1, 1)),
        (
            "date.truncate('weekYear', date({year: 1984, month: 2, day: 1}), {day: 5})".to_string(),
            date(1984, 1, 5),
        ),
        (
            "date.truncate('weekYear', localdatetime({year: 1984, month: 1, day: 1}), {})"
                .to_string(),
            date(1983, 1, 3),
        ),
        (
            format!("date.truncate('quarter', {ldt}, {{day: 2}})"),
            date(1984, 10, 2),
        ),
        (
            format!("date.truncate('week', {ldt}, {{dayOfWeek: 2}})"),
            date(1984, 10, 9),
        ),
        (
            format!("date.truncate('week', {ldt}, {{}})"),
            date(1984, 10, 8),
        ),
        (format!("date.truncate('day', {ldt})"), date(1984, 10, 11)),
        (
            "localdatetime.truncate('month', date({year: 1984, month: 10, day: 11}), {day: 2})"
                .to_string(),
            local_datetime(1984, 10, 2, 0, 0, 0, 0),
        ),
        (
            format!("localdatetime.truncate('hour', {ldt}, {{nanosecond: 2}})"),
            local_datetime(1984, 10, 11, 12, 0, 0, 2),
        ),
        (
            format!("localdatetime.truncate('millisecond', {ldt})"),
            local_datetime(1984, 10, 11, 12, 31, 14, 645_000_000),
        ),
        (
            format!("localdatetime.truncate('microsecond', {ldt})"),
            local_datetime(1984, 10, 11, 12, 31, 14, 645_876_000),
        ),
        (
            format!("datetime.truncate('day', {ldt}, {{hour: 3}})"),
            Value::ZonedDateTime {
                utc: chrono::NaiveDate::from_ymd_opt(1984, 10, 11)
                    .unwrap()
                    .and_hms_opt(3, 0, 0)
                    .unwrap(),
                offset_secs: 0,
            },
        ),
    ] {
        assert_eq!(cell(&format!("RETURN {query}")), expected, "{query}");
    }
    for (query, expected) in [
        ("localtime.truncate('hour', localtime({hour: 12, minute: 31, second: 14, nanosecond: 645876123}), {nanosecond: 2})", "12:00:00.000000002"),
        ("localtime.truncate('day', localdatetime({year: 1984, month: 10, day: 11, hour: 12}), {})", "00:00"),
        ("localtime.truncate('minute', localtime('12:31:14.645'))", "12:31"),
    ] {
        assert_eq!(
            cell(&format!("RETURN {query}")),
            local_time(expected),
            "{query}"
        );
    }
    assert_eq!(
        cell("RETURN time.truncate('second', localtime('12:31:14.645'))"),
        zoned_time("12:31:14", 0)
    );
    assert_eq!(
        cell("RETURN time.truncate('minute', time('12:31:14+01:00'))"),
        zoned_time("12:31", 3600)
    );
}

#[test]
fn truncate_refuses_a_unit_the_type_cannot_hold() {
    for query in [
        "RETURN date.truncate('hour', date('1984-10-11'))",
        "RETURN localtime.truncate('year', localtime('12:31'))",
        "RETURN date.truncate('fortnight', date('1984-10-11'))",
        "RETURN date.truncate(5, date('1984-10-11'))",
        "RETURN date.truncate('day', 5)",
    ] {
        assert!(run(query).is_err(), "{query}");
    }
    assert_eq!(cell("RETURN date.truncate('day', null)"), Value::Null);
}

fn duration(months: i32, days: i32, seconds: i64) -> Value {
    Value::Duration {
        months,
        days,
        seconds,
        nanos: 0,
    }
}

#[test]
fn duration_components_spill_fractions_and_parse_text() {
    for (query, expected) in [
        (
            "duration({days: 14, hours: 16, minutes: 12})",
            duration(0, 14, 58_320),
        ),
        ("duration({months: 5, days: 1.5})", duration(5, 1, 43_200)),
        ("duration({weeks: 2.5})", duration(0, 17, 43_200)),
        ("duration({minutes: 1.5, seconds: 1})", duration(0, 0, 91)),
        (
            "duration({years: 12, months: 5, days: 14, hours: 16, minutes: 12, seconds: 70})",
            duration(149, 14, 58_390),
        ),
        (
            "duration({seconds: 70, milliseconds: 1000})",
            duration(0, 0, 71),
        ),
        ("duration('P14DT16H12M')", duration(0, 14, 58_320)),
        ("duration('P5M1.5D')", duration(5, 1, 43_200)),
        ("duration('PT0.75M')", duration(0, 0, 45)),
        ("duration('P2.5W')", duration(0, 17, 43_200)),
        (
            "duration('P2012-02-02T14:37:21')",
            duration(2012 * 12 + 2, 2, 52_641),
        ),
    ] {
        assert_eq!(cell(&format!("RETURN {query}")), expected, "{query}");
    }
    for query in [
        "RETURN duration('P1.5S')",
        "RETURN duration('14D')",
        "RETURN duration('P1D1Y')",
    ] {
        assert!(run(query).is_err(), "{query}");
    }
    assert_eq!(cell("RETURN duration(null)"), Value::Null);
}

#[test]
fn duration_to_string_is_iso_8601() {
    for (query, expected) in [
        (
            "duration({days: 14, hours: 16, minutes: 12})",
            "P14DT16H12M",
        ),
        (
            "duration({years: 12, months: 5, days: 14, hours: 16, minutes: 12, seconds: 70})",
            "P12Y5M14DT16H13M10S",
        ),
        ("duration({seconds: -79200})", "PT-22H"),
        ("duration({})", "PT0S"),
    ] {
        assert_eq!(
            cell(&format!("RETURN toString({query})")),
            text(expected),
            "{query}"
        );
    }
}

#[test]
fn duration_in_units_counts_whole_units() {
    for (query, expected) in [
        ("duration.inMonths(date('1984-10-11'), date('2015-06-24'))", duration(368, 0, 0)),
        ("duration.inMonths(date('2015-06-24'), date('1984-10-11'))", duration(-368, 0, 0)),
        ("duration.inMonths(localdatetime('2015-07-21T21:40:32.142'), localdatetime('2016-07-21T21:45:22.142'))", duration(12, 0, 0)),
        ("duration.inDays(date('1984-10-11'), date('2015-06-24'))", duration(0, 11_213, 0)),
        ("duration.inSeconds(localdatetime('2018-01-01T12:00'), localdatetime('2018-01-02T10:00'))", duration(0, 0, 79_200)),
        ("duration.inSeconds(localdatetime('2018-01-02T10:00'), localdatetime('2018-01-01T12:00'))", duration(0, 0, -79_200)),
    ] {
        assert_eq!(cell(&format!("RETURN {query}")), expected, "{query}");
    }
    assert_eq!(
        cell("RETURN duration.inDays(null, date('2015-06-24'))"),
        Value::Null
    );
    assert!(run("RETURN duration.inDays(1, date('2015-06-24'))").is_err());
}

#[test]
fn component_accessors() {
    let ints = |query: &str| -> Vec<i64> {
        match cell(query) {
            Value::List(items) => items
                .into_iter()
                .map(|v| match v {
                    Value::Int64(n) => n,
                    other => panic!("{query}: {other:?}"),
                })
                .collect(),
            other => panic!("{query}: {other:?}"),
        }
    };
    assert_eq!(
        ints("WITH date({year: 1984, month: 10, day: 11}) AS d RETURN [d.year, d.quarter, d.month, d.week, d.weekYear, d.day, d.ordinalDay, d.weekDay, d.dayOfQuarter]"),
        [1984, 4, 10, 41, 1984, 11, 285, 4, 11]
    );
    assert_eq!(
        ints("WITH date({year: 1984, month: 1, day: 1}) AS d RETURN [d.year, d.weekYear, d.week, d.weekDay]"),
        [1984, 1983, 52, 7]
    );
    assert_eq!(
        ints("WITH localdatetime({year: 1984, month: 11, day: 11, hour: 12, minute: 31, second: 14, nanosecond: 645876123}) AS d RETURN [d.quarter, d.week, d.dayOfQuarter, d.hour, d.minute, d.second, d.millisecond, d.microsecond, d.nanosecond]"),
        [4, 45, 42, 12, 31, 14, 645, 645876, 645876123]
    );
    assert_eq!(
        ints("WITH duration({years: 1, months: 4, days: 10, hours: 1, minutes: 1, seconds: 1}) AS d RETURN [d.years, d.quarters, d.months, d.weeks, d.days, d.hours, d.minutes, d.seconds, d.quartersOfYear, d.monthsOfQuarter, d.monthsOfYear, d.daysOfWeek, d.minutesOfHour, d.secondsOfMinute]"),
        [1, 5, 16, 1, 10, 1, 61, 3661, 1, 1, 4, 3, 1, 1]
    );
    assert_eq!(
        cell("WITH date('2015-07-21') AS d RETURN d.nothing"),
        Value::Null
    );
    assert_eq!(
        cell("RETURN localdatetime('2015-07-21T21:40:32').nothing"),
        Value::Null
    );
}

fn fine(months: i32, days: i32, seconds: i64, nanos: i32) -> Value {
    Value::Duration {
        months,
        days,
        seconds,
        nanos,
    }
}

#[test]
fn sub_second_durations_keep_every_nanosecond() {
    for (query, expected) in [
        ("duration({nanoseconds: 1})", fine(0, 0, 0, 1)),
        ("duration({microseconds: 1})", fine(0, 0, 0, 1_000)),
        ("duration({milliseconds: 1})", fine(0, 0, 0, 1_000_000)),
        (
            "duration({seconds: 1, milliseconds: 1})",
            fine(0, 0, 1, 1_000_000),
        ),
        ("duration({days: 14, nanoseconds: 1})", fine(0, 14, 0, 1)),
        ("duration({seconds: 1.5})", fine(0, 0, 1, 500_000_000)),
        // Neo4j normalisation: the sign rides on `seconds`, nanos stay >= 0.
        ("duration({seconds: -0.5})", fine(0, 0, -1, 500_000_000)),
        ("duration({nanoseconds: -1})", fine(0, 0, -1, 999_999_999)),
        (
            "duration({milliseconds: 1500, microseconds: 7})",
            fine(0, 0, 1, 500_007_000),
        ),
        ("duration('PT0.000000001S')", fine(0, 0, 0, 1)),
        ("duration('PT1.5S')", fine(0, 0, 1, 500_000_000)),
        ("duration('PT-0.5S')", fine(0, 0, -1, 500_000_000)),
        ("duration('P1DT0.25S')", fine(0, 1, 0, 250_000_000)),
    ] {
        assert_eq!(cell(&format!("RETURN {query}")), expected, "{query}");
    }
}

#[test]
fn sub_second_duration_to_string_is_iso_8601() {
    for (query, expected) in [
        ("duration({nanoseconds: 1})", "PT0.000000001S"),
        ("duration({milliseconds: 1500})", "PT1.5S"),
        ("duration({seconds: -0.5})", "PT-0.5S"),
        ("duration({minutes: -1, seconds: -30.75})", "PT-1M-30.75S"),
        (
            "duration({days: 2, hours: 1, milliseconds: 40})",
            "P2DT1H0.04S",
        ),
    ] {
        assert_eq!(
            cell(&format!("RETURN toString({query})")),
            text(expected),
            "{query}"
        );
    }
}

#[test]
fn sub_second_duration_arithmetic_carries_nanoseconds() {
    let stamp = |nanos| local_datetime(2015, 7, 21, 21, 40, 32, nanos);
    for (query, expected) in [
        (
            "localdatetime('2015-07-21T21:40:32.142') + duration({nanoseconds: 1})",
            stamp(142_000_001),
        ),
        (
            "duration({nanoseconds: 1}) + localdatetime('2015-07-21T21:40:32.142')",
            stamp(142_000_001),
        ),
        (
            "localdatetime('2015-07-21T21:40:32.142') - duration({milliseconds: 142})",
            stamp(0),
        ),
        (
            "localdatetime('2015-07-21T21:40:32.142') + duration({milliseconds: 900})",
            local_datetime(2015, 7, 21, 21, 40, 33, 42_000_000),
        ),
        (
            "localdatetime('2015-07-21T21:40:32.142') - duration({milliseconds: 900})",
            local_datetime(2015, 7, 21, 21, 40, 31, 242_000_000),
        ),
        (
            "duration({nanoseconds: 600000000}) + duration({nanoseconds: 600000000})",
            fine(0, 0, 1, 200_000_000),
        ),
        (
            "duration({seconds: 1}) - duration({nanoseconds: 1})",
            fine(0, 0, 0, 999_999_999),
        ),
        (
            "duration({nanoseconds: 400000000}) * 3",
            fine(0, 0, 1, 200_000_000),
        ),
        (
            "3 * duration({days: 1, nanoseconds: 400000000})",
            fine(0, 3, 1, 200_000_000),
        ),
        (
            "localdatetime('2015-07-21T21:40:32.142') - localdatetime('2015-07-21T21:40:31.141')",
            fine(0, 0, 1, 1_000_000),
        ),
        (
            "localdatetime('2015-07-21T21:40:31.141') - localdatetime('2015-07-21T21:40:32.142')",
            fine(0, 0, -2, 999_000_000),
        ),
    ] {
        assert_eq!(cell(&format!("RETURN {query}")), expected, "{query}");
    }
    // A date has no time of day, so a sub-second duration does not move it.
    assert_eq!(
        cell("RETURN date('2015-07-21') + duration({nanoseconds: 5})"),
        date(2015, 7, 21)
    );
    assert!(run("RETURN duration({seconds: 9223372036854775807, nanoseconds: 999999999}) + duration({nanoseconds: 1})").is_err());
}

#[test]
fn sub_second_duration_equality_accessors_and_between() {
    assert_eq!(
        cell("RETURN duration({nanoseconds: 1}) = duration({nanoseconds: 1})"),
        Value::Boolean(true)
    );
    assert_eq!(
        cell("RETURN duration({nanoseconds: 1}) = duration({nanoseconds: 2})"),
        Value::Boolean(false)
    );
    assert_eq!(
        cell("RETURN duration({seconds: 1}) = duration({seconds: 1, nanoseconds: 1})"),
        Value::Boolean(false)
    );
    let ints = |query: &str| -> Vec<i64> {
        match cell(query) {
            Value::List(items) => items
                .into_iter()
                .map(|v| match v {
                    Value::Int64(n) => n,
                    other => panic!("{query}: {other:?}"),
                })
                .collect(),
            other => panic!("{query}: {other:?}"),
        }
    };
    assert_eq!(
        ints("WITH duration({seconds: 2, nanoseconds: 345678901}) AS d RETURN [d.seconds, d.milliseconds, d.microseconds, d.nanoseconds, d.millisecondsOfSecond, d.microsecondsOfSecond, d.nanosecondsOfSecond]"),
        [2, 2345, 2_345_678, 2_345_678_901, 345, 345_678, 345_678_901]
    );
    // -0.5 s is (-1 s, +500_000_000 ns): `.seconds` is the floor, the unit totals are exact.
    assert_eq!(
        ints("WITH duration({seconds: -0.5}) AS d RETURN [d.seconds, d.milliseconds, d.nanoseconds, d.nanosecondsOfSecond]"),
        [-1, -500, -500_000_000, 500_000_000]
    );
    for (query, expected) in [
        (
            "duration.between(localdatetime('2015-07-21T21:40:32.142'), localdatetime('2015-07-22T21:40:32.143'))",
            fine(0, 1, 0, 1_000_000),
        ),
        (
            "duration.inSeconds(localdatetime('2015-07-21T21:40:32.142'), localdatetime('2015-07-21T21:40:33.000'))",
            fine(0, 0, 0, 858_000_000),
        ),
        (
            "duration.inSeconds(localdatetime('2015-07-21T21:40:33.000'), localdatetime('2015-07-21T21:40:32.142'))",
            fine(0, 0, -1, 142_000_000),
        ),
    ] {
        assert_eq!(cell(&format!("RETURN {query}")), expected, "{query}");
    }
}
