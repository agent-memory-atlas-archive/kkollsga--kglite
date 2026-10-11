//! Absolute goldens for `LocalTime` and `Time`: constructors, accessors,
//! comparison, equality, arithmetic, `duration.between` and text. The
//! differential corpus cannot see these (an executor or parser defect is
//! shared by the optimised and unoptimised paths), so every expectation is a
//! value written down by hand.

use super::temporal_functions::{cell, run};
use super::*;

fn local(clock: &str) -> Value {
    let time = crate::datatypes::time_of_day::parse_clock(clock).unwrap();
    Value::LocalTime(crate::datatypes::time_of_day::nanos_of(time))
}

fn zoned(clock: &str, offset_secs: i32) -> Value {
    let time = crate::datatypes::time_of_day::parse_clock(clock).unwrap();
    Value::Time {
        nanos: crate::datatypes::time_of_day::nanos_of(time),
        offset_secs,
    }
}

fn text(s: &str) -> Value {
    Value::String(s.to_string())
}

fn int(n: i64) -> Value {
    Value::Int64(n)
}

fn check(cases: &[(&str, Value)]) {
    for (query, expected) in cases {
        assert_eq!(&cell(&format!("RETURN {query}")), expected, "{query}");
    }
}

fn rows(query: &str) -> Vec<Value> {
    let graph = DirGraph::new();
    let parsed = parser::parse_cypher(query).unwrap();
    let params = HashMap::new();
    let result = CypherExecutor::with_params(&graph, &params, None)
        .execute(&parsed)
        .unwrap_or_else(|e| panic!("query failed: {query}\n  error: {e}"));
    result.rows.into_iter().map(|mut r| r.remove(0)).collect()
}

#[test]
fn constructors_read_the_iso_spellings_and_offsets() {
    check(&[
        ("time('21:40:32.142+01:00')", zoned("21:40:32.142", 3600)),
        ("time('214032.142+0100')", zoned("21:40:32.142", 3600)),
        ("time('21:40:32Z')", zoned("21:40:32", 0)),
        ("time('2140-0530')", zoned("21:40", -19_800)),
        ("time('21')", zoned("21:00", 0)),
        ("time('21:40')", zoned("21:40", 0)),
        (
            "time({hour: 12, minute: 31, timezone: '+01:00'})",
            zoned("12:31", 3600),
        ),
        ("time({hour: 12, timezone: 'Z'})", zoned("12:00", 0)),
        (
            "time({time: time('10:00+02:00'), minute: 30})",
            zoned("10:30", 7200),
        ),
        (
            // A zoned base moves to the new offset (same instant), as in Neo4j.
            "time({time: time('10:00+02:00'), timezone: '-01:00'})",
            zoned("07:00", -3600),
        ),
        ("time(localtime('10:00'))", zoned("10:00", 0)),
        ("time(time('10:00+01:00'))", zoned("10:00", 3600)),
        ("localtime(time('10:00+01:00'))", local("10:00")),
        ("localtime('10:00:00.5')", local("10:00:00.5")),
        (
            "localtime({hour: 10, minute: 15, nanosecond: 7})",
            local("10:15:00.000000007"),
        ),
        (
            "localtime({time: time('10:00+02:00'), second: 5})",
            local("10:00:05"),
        ),
        ("time(null)", Value::Null),
        ("localtime(null)", Value::Null),
    ]);
    // `time()` with no argument is the UTC clock; `localtime()` the local one.
    assert!(matches!(
        cell("RETURN time()"),
        Value::Time { offset_secs: 0, .. }
    ));
    assert!(matches!(cell("RETURN localtime()"), Value::LocalTime(_)));
}

#[test]
fn constructors_refuse_what_they_cannot_hold() {
    for query in [
        "RETURN time('10:00[Europe/Oslo]')",
        "RETURN time({hour: 10, timezone: 'Europe/Oslo'})",
        "RETURN time('10:00+19:00')",
        "RETURN time('25:00')",
        "RETURN time('nonsense')",
        "RETURN localtime('10:00:61')",
        "RETURN localtime(5)",
        "RETURN time({hour: 10, bogus: 1})",
        "RETURN localtime({hour: 10, timezone: '+01:00'})",
    ] {
        assert!(run(query).is_err(), "{query}");
    }
}

#[test]
fn accessors_read_the_clock_and_the_offset() {
    check(&[
        ("time('21:40:32.142857+01:00').hour", int(21)),
        ("time('21:40:32.142857+01:00').minute", int(40)),
        ("time('21:40:32.142857+01:00').second", int(32)),
        ("time('21:40:32.142857+01:00').millisecond", int(142)),
        ("time('21:40:32.142857+01:00').microsecond", int(142_857)),
        (
            "time('21:40:32.142857123+01:00').nanosecond",
            int(142_857_123),
        ),
        ("time('21:40+01:00').offset", text("+01:00")),
        ("time('21:40+01:30').offsetMinutes", int(90)),
        ("time('21:40-05:00').offsetSeconds", int(-18_000)),
        ("time('21:40Z').offset", text("Z")),
        ("time('21:40+01:00').timezone", text("+01:00")),
        ("localtime('21:40:32.142857').microsecond", int(142_857)),
        ("localtime('21:40').offset", Value::Null),
        ("localtime('21:40').timezone", Value::Null),
        ("localtime('21:40').year", Value::Null),
    ]);
}

#[test]
fn text_is_the_shortest_iso_form() {
    check(&[
        (
            "toString(time('21:40:32.142+01:00'))",
            text("21:40:32.142+01:00"),
        ),
        ("toString(time('21:40:00Z'))", text("21:40Z")),
        (
            "toString(time('21:40:32.000000003-05:30'))",
            text("21:40:32.000000003-05:30"),
        ),
        ("toString(localtime('21:40:32.5'))", text("21:40:32.500")),
        ("toString(localtime('21:40:00'))", text("21:40")),
        ("'t=' + toString(localtime('09:05'))", text("t=09:05")),
    ]);
}

#[test]
fn zoned_times_compare_by_their_utc_reading() {
    let yes = Value::Boolean(true);
    let no = Value::Boolean(false);
    check(&[
        // 10:00+01:00 is 09:00Z, which is before 09:35:14Z.
        ("time('10:00+01:00') < time('09:35:14Z')", yes.clone()),
        ("time('10:00+01:00') > time('09:35:14Z')", no.clone()),
        ("time('10:00+01:00') <= time('09:00Z')", yes.clone()),
        ("time('10:00+01:00') >= time('09:00Z')", yes.clone()),
        // Equality is instant-of-day equality: offsets are not identity.
        ("time('10:00+01:00') = time('09:00Z')", yes.clone()),
        ("time('10:00+01:00') <> time('09:00Z')", no.clone()),
        ("time('10:00+01:00') = time('10:00Z')", no.clone()),
        ("time('23:30-02:00') > time('23:59Z')", yes.clone()),
        ("time('00:30+01:00') < time('00:00Z')", yes.clone()),
        (
            "localtime('10:00') < localtime('10:00:00.000000001')",
            yes.clone(),
        ),
        ("localtime('10:00') = localtime('10')", yes),
        ("localtime('10:00') >= localtime('11:00')", no),
    ]);
}

#[test]
fn local_and_zoned_times_and_dates_are_incomparable() {
    let no = Value::Boolean(false);
    check(&[
        ("localtime('10:00') = time('10:00Z')", no.clone()),
        ("localtime('10:00') < time('11:00Z')", Value::Null),
        ("time('10:00Z') > localtime('09:00')", Value::Null),
        ("localtime('10:00') = date('2020-01-01')", no.clone()),
        ("localtime('10:00') < date('2020-01-01')", Value::Null),
        (
            "time('10:00Z') < localdatetime('2020-01-01T11:00')",
            Value::Null,
        ),
        ("localtime('10:00') = '10:00'", no),
        ("localtime('10:00') < '11:00'", Value::Null),
    ]);
}

#[test]
fn membership_distinct_and_ordering_agree_with_equality() {
    let yes = Value::Boolean(true);
    let no = Value::Boolean(false);
    check(&[
        ("time('10:00+01:00') IN [time('09:00Z'), 7]", yes.clone()),
        ("time('10:00+01:00') IN [time('10:00Z')]", no.clone()),
        ("localtime('10:00') IN [localtime('10'), 1]", yes),
        ("localtime('10:00') IN [time('10:00Z')]", no),
    ]);
    // DISTINCT merges the same instant of day and keeps the first offset.
    assert_eq!(
        rows("UNWIND [time('10:00+01:00'), time('09:00Z'), time('09:30Z')] AS t RETURN DISTINCT t"),
        vec![zoned("10:00", 3600), zoned("09:30", 0)]
    );
    // ORDER BY: UTC order for zoned times, clock order for local ones.
    assert_eq!(
        rows(
            "UNWIND [time('10:00+01:00'), time('08:30Z'), time('09:45Z')] AS t RETURN t ORDER BY t"
        ),
        vec![zoned("08:30", 0), zoned("10:00", 3600), zoned("09:45", 0)]
    );
    assert_eq!(
        rows("UNWIND [localtime('12:00'), localtime('01:00'), localtime('06:00')] AS t RETURN t ORDER BY t DESC"),
        vec![local("12:00"), local("06:00"), local("01:00")]
    );
    // min / max keep the order's extremes.
    assert_eq!(
        rows("UNWIND [localtime('12:00'), localtime('01:00')] AS t RETURN max(t)"),
        vec![local("12:00")]
    );
    assert_eq!(
        rows("UNWIND [time('10:00+01:00'), time('09:30Z')] AS t RETURN min(t)"),
        vec![zoned("10:00", 3600)]
    );
    // Grouping keys on the instant of day: two of the three share a group.
    assert_eq!(
        rows("UNWIND [time('10:00+01:00'), time('09:00Z'), time('09:30Z')] AS t WITH t, count(*) AS n RETURN n ORDER BY n DESC LIMIT 1"),
        vec![int(2)]
    );
}

#[test]
fn arithmetic_moves_the_clock_part_modulo_a_day() {
    check(&[
        ("localtime('23:30') + duration({hours: 1})", local("00:30")),
        ("duration({hours: 1}) + localtime('23:30')", local("00:30")),
        ("localtime('00:10') - duration('PT20M')", local("23:50")),
        (
            "localtime('10:00') + duration({days: 3, months: 2})",
            local("10:00"),
        ),
        (
            "localtime('10:00') + duration({hours: 1, nanoseconds: 5})",
            local("11:00:00.000000005"),
        ),
        ("localtime('10:00') - duration({hours: 25})", local("09:00")),
        (
            "time('10:00+01:00') + duration({minutes: 90, days: 9})",
            zoned("11:30", 3600),
        ),
        (
            "time('00:30-02:00') - duration({hours: 1})",
            zoned("23:30", -7200),
        ),
        ("localtime('10:00') + null", Value::Null),
    ]);
}

#[test]
fn duration_between_two_times_is_seconds_and_nanoseconds() {
    let span = |seconds: i64, nanos: i32| Value::Duration {
        months: 0,
        days: 0,
        seconds,
        nanos,
    };
    check(&[
        (
            "duration.between(localtime('10:00'), localtime('12:30:15.5'))",
            span(9015, 500_000_000),
        ),
        (
            "duration.between(localtime('12:00'), localtime('10:00'))",
            span(-7200, 0),
        ),
        (
            "duration.between(time('10:00+01:00'), time('10:00Z'))",
            span(3600, 0),
        ),
        (
            "duration.between(localtime('10:00'), time('11:00+05:00'))",
            span(3600, 0),
        ),
        ("duration.between(localtime('10:00'), null)", Value::Null),
    ]);
    assert!(run("RETURN duration.between(localtime('10:00'), date('2020-01-01'))").is_err());
}

#[test]
fn truncation_keeps_the_offset() {
    check(&[
        (
            "time.truncate('hour', time('12:31:14.5+02:00'))",
            zoned("12:00", 7200),
        ),
        (
            "time.truncate('millisecond', time('12:31:14.645876123-01:00'))",
            zoned("12:31:14.645", -3600),
        ),
        (
            "time.truncate('minute', time('12:31:14+02:00'), {timezone: 'Z'})",
            zoned("12:31", 0),
        ),
        (
            "localtime.truncate('hour', time('12:31:14+02:00'))",
            local("12:00"),
        ),
    ]);
}
