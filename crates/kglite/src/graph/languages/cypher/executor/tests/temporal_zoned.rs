//! Absolute goldens for `ZonedDateTime`: constructors, accessors, text,
//! comparison against the local kinds, arithmetic, `duration.between`,
//! truncation and storage. Zoned equality ignores the offset, so a result is
//! compared on its `Debug` form wherever the offset is the point; every
//! expectation is written down by hand.

use super::temporal_functions::{cell, run};
use super::*;

/// The value an RFC 3339 stamp names, built by chrono, not by the engine.
fn at(rfc3339: &str) -> Value {
    let parsed = chrono::DateTime::parse_from_rfc3339(rfc3339).unwrap();
    Value::ZonedDateTime {
        utc: parsed.naive_utc(),
        offset_secs: parsed.offset().local_minus_utc(),
        zone: None,
    }
}

fn text(s: &str) -> Value {
    Value::String(s.to_string())
}

fn int(n: i64) -> Value {
    Value::Int64(n)
}

fn local(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> Value {
    Value::Timestamp(
        chrono::NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap(),
    )
}

/// Compare on `Debug`, which carries the offset that `==` ignores.
fn check(cases: &[(&str, Value)]) {
    for (query, expected) in cases {
        let got = cell(&format!("RETURN {query}"));
        assert_eq!(format!("{got:?}"), format!("{expected:?}"), "{query}");
    }
}

#[test]
fn constructors_keep_the_offset_and_default_to_utc() {
    check(&[
        (
            "datetime('2015-07-21T21:40:32.142+01:00')",
            at("2015-07-21T21:40:32.142+01:00"),
        ),
        (
            "datetime('2015-07-21T21:40:32.142Z')",
            at("2015-07-21T21:40:32.142Z"),
        ),
        ("datetime('2015-07-21T21:40')", at("2015-07-21T21:40:00Z")),
        // No offset reads as UTC; a bare date is midnight UTC.
        (
            "datetime('2015-07-21T21:40:32')",
            at("2015-07-21T21:40:32Z"),
        ),
        ("datetime('2015-07-21')", at("2015-07-21T00:00:00Z")),
        // Other ISO spellings, compact offsets included.
        (
            "datetime('20150721T214032+0100')",
            at("2015-07-21T21:40:32+01:00"),
        ),
        (
            "datetime('2015-W30-2T21:40+01:30')",
            at("2015-07-21T21:40:00+01:30"),
        ),
        (
            "datetime('2015-07-21T21:40:32-05:30')",
            at("2015-07-21T21:40:32-05:30"),
        ),
        (
            "datetime({year: 2015, month: 7, day: 21, hour: 21, timezone: '+01:00'})",
            at("2015-07-21T21:00:00+01:00"),
        ),
        (
            "datetime({year: 2015, month: 7, day: 21, hour: 21})",
            at("2015-07-21T21:00:00Z"),
        ),
        // A base value keeps its offset unless `timezone` overrides it.
        (
            "datetime({datetime: datetime('2015-07-21T21:40:32+01:00'), minute: 5})",
            at("2015-07-21T21:05:32+01:00"),
        ),
        (
            // A zoned base moves to the new zone (same instant), as in Neo4j.
            "datetime({datetime: datetime('2015-07-21T21:40:32+01:00'), timezone: '-02:00'})",
            at("2015-07-21T18:40:32-02:00"),
        ),
        (
            "datetime({date: date('2015-07-21'), time: time('10:30+02:00')})",
            at("2015-07-21T10:30:00+02:00"),
        ),
        // A local reading and a date name the same wall clock at UTC.
        (
            "datetime(localdatetime('2015-07-21T21:40:32'))",
            at("2015-07-21T21:40:32Z"),
        ),
        ("datetime(date('2015-07-21'))", at("2015-07-21T00:00:00Z")),
        (
            "datetime(datetime('2015-07-21T21:40+03:00'))",
            at("2015-07-21T21:40:00+03:00"),
        ),
        ("datetime(null)", Value::Null),
        // The other constructors read the wall clock a zoned value was written in.
        (
            "date(datetime('2015-07-21T23:40+05:00'))",
            Value::DateTime(chrono::NaiveDate::from_ymd_opt(2015, 7, 21).unwrap()),
        ),
        (
            "localdatetime(datetime('2015-07-21T23:40+05:00'))",
            local(2015, 7, 21, 23, 40, 0),
        ),
        (
            "time(datetime('2015-07-21T23:40+05:00'))",
            Value::Time {
                nanos: 85_200_000_000_000,
                offset_secs: 18_000,
            },
        ),
        (
            "localtime(datetime('2015-07-21T23:40+05:00'))",
            Value::LocalTime(85_200_000_000_000),
        ),
    ]);
    // `datetime()` is the UTC clock.
    assert!(matches!(
        cell("RETURN datetime()"),
        Value::ZonedDateTime { offset_secs: 0, .. }
    ));
    // `localdatetime()` stays the local kind.
    assert!(matches!(
        cell("RETURN localdatetime()"),
        Value::Timestamp(_)
    ));
}

#[test]
fn constructors_refuse_what_they_cannot_hold() {
    for query in [
        "RETURN datetime('2015-07-21T21:40[Mars/Base]')",
        "RETURN datetime({year: 2015, timezone: 'Mars/Base'})",
        "RETURN datetime({year: 2015, timezone: '+25:00'})",
        "RETURN datetime('2015-07-21T21:40+19:00')",
        "RETURN datetime('2015-07-21T25:00Z')",
        "RETURN datetime('nonsense')",
        "RETURN datetime(5)",
        "RETURN datetime({year: 2015, bogus: 1})",
        "RETURN localdatetime({year: 2015, timezone: '+01:00'})",
    ] {
        assert!(run(query).is_err(), "{query}");
    }
}

#[test]
fn accessors_read_the_wall_clock_and_the_instant() {
    let z = "datetime('2015-07-21T21:40:32.142857123+01:00')";
    // 2015-07-21T20:40:32Z is 1_437_511_232 seconds after the epoch.
    let cases = [
        ("{z}.year", int(2015)),
        ("{z}.month", int(7)),
        ("{z}.day", int(21)),
        ("{z}.hour", int(21)),
        ("{z}.minute", int(40)),
        ("{z}.second", int(32)),
        ("{z}.millisecond", int(142)),
        ("{z}.microsecond", int(142_857)),
        ("{z}.nanosecond", int(142_857_123)),
        ("{z}.week", int(30)),
        ("{z}.offset", text("+01:00")),
        ("{z}.timezone", text("+01:00")),
        ("{z}.offsetSeconds", int(3600)),
        ("{z}.offsetMinutes", int(60)),
        ("{z}.epochSeconds", int(1_437_511_232)),
        ("{z}.epochMillis", int(1_437_511_232_142)),
        ("datetime('2015-07-21T21:40Z').offset", text("Z")),
        (
            "datetime('2015-07-21T21:40-05:30').offsetMinutes",
            int(-330),
        ),
        // The date rolls with the wall clock, not the instant.
        ("datetime('2015-07-21T23:30-05:00').day", int(21)),
        (
            "datetime('2015-07-21T23:30-05:00').epochSeconds",
            int(1_437_539_400),
        ),
        ("datetime('2015-07-21T21:40Z').nope", Value::Null),
    ];
    for (query, expected) in cases {
        let query = query.replace("{z}", z);
        let got = cell(&format!("RETURN {query}"));
        assert_eq!(format!("{got:?}"), format!("{expected:?}"), "{query}");
    }
}

#[test]
fn text_is_the_wall_clock_with_its_offset() {
    check(&[
        (
            "toString(datetime('2015-07-21T21:40:32.142+01:00'))",
            text("2015-07-21T21:40:32.142+01:00"),
        ),
        (
            "toString(datetime('2015-07-21T20:40:32.142Z'))",
            text("2015-07-21T20:40:32.142Z"),
        ),
        (
            "toString(datetime('1984-10-11T12:31:00Z'))",
            text("1984-10-11T12:31Z"),
        ),
        (
            "toString(datetime('1984-10-11T12:31:14.000000003-05:30'))",
            text("1984-10-11T12:31:14.000000003-05:30"),
        ),
        (
            "'at ' + toString(datetime('2015-07-21T21:40Z'))",
            text("at 2015-07-21T21:40Z"),
        ),
    ]);
}

#[test]
fn comparison_is_by_instant_and_reads_a_local_datetime_as_utc() {
    let yes = Value::Boolean(true);
    let no = Value::Boolean(false);
    check(&[
        // One instant at two offsets.
        (
            "datetime('2015-07-21T21:40+01:00') = datetime('2015-07-21T20:40Z')",
            yes.clone(),
        ),
        (
            "datetime('2015-07-21T21:40+01:00') <> datetime('2015-07-21T20:40Z')",
            no.clone(),
        ),
        (
            "datetime('2015-07-21T21:40+01:00') = datetime('2015-07-21T21:40Z')",
            no.clone(),
        ),
        (
            "datetime('2015-07-21T21:40+01:00') < datetime('2015-07-21T21:40Z')",
            yes.clone(),
        ),
        (
            "datetime('2015-07-21T21:40+01:00') >= datetime('2015-07-21T20:40Z')",
            yes.clone(),
        ),
        (
            "datetime('2015-07-21T21:40+01:00') > datetime('2015-07-21T20:41Z')",
            no.clone(),
        ),
        // A local datetime is read as UTC: the engine stores every local stamp as one.
        (
            "datetime('2015-07-21T20:40Z') = localdatetime('2015-07-21T20:40')",
            yes.clone(),
        ),
        (
            "localdatetime('2015-07-21T20:40') = datetime('2015-07-21T21:40+01:00')",
            yes.clone(),
        ),
        (
            "datetime('2015-07-21T21:40+01:00') < localdatetime('2015-07-21T21:40')",
            yes.clone(),
        ),
        (
            "localdatetime('2015-07-21T21:40') > datetime('2015-07-21T21:40+01:00')",
            yes.clone(),
        ),
        // A date is its midnight UTC.
        (
            "datetime('2015-07-21T02:00+02:00') = date('2015-07-21')",
            yes.clone(),
        ),
        (
            "date('2015-07-21') < datetime('2015-07-21T00:00:01Z')",
            yes.clone(),
        ),
        (
            "datetime('2015-07-20T23:59:59Z') < date('2015-07-21')",
            yes.clone(),
        ),
        // Text parses as it does against the local kinds.
        (
            "datetime('2015-07-21T20:40:00Z') = '2015-07-21T20:40:00'",
            yes.clone(),
        ),
        (
            "datetime('2015-07-21T20:40:00Z') < '2015-07-22'",
            yes.clone(),
        ),
        // Other families stay incomparable.
        ("datetime('2015-07-21T20:40Z') = time('20:40Z')", no.clone()),
        (
            "datetime('2015-07-21T20:40Z') < time('21:40Z')",
            Value::Null,
        ),
        ("datetime('2015-07-21T20:40Z') = 1", no),
        // Membership agrees with equality, hashed or linear.
        (
            "datetime('2015-07-21T21:40+01:00') IN [datetime('2015-07-21T20:40Z'), 7]",
            yes.clone(),
        ),
        (
            "localdatetime('2015-07-21T20:40') IN [datetime('2015-07-21T21:40+01:00')]",
            yes.clone(),
        ),
        (
            "datetime('2015-07-21T21:40Z') IN [datetime('2015-07-21T21:40+01:00')]",
            Value::Boolean(false),
        ),
        ("date('2015-07-21') IN [datetime('2015-07-21T00:00Z')]", yes),
    ]);
}

#[test]
fn a_zoned_now_compares_with_a_stored_local_timestamp() {
    // The day `datetime()` turns zoned must not empty `WHERE n.at < datetime()`:
    // every stored local stamp is a UTC instant.
    let mut graph = DirGraph::new();
    for q in [
        "CREATE (:Ev {k: 'past', at: localdatetime('2000-01-01T00:00:00')})",
        "CREATE (:Ev {k: 'future', at: localdatetime('2999-01-01T00:00:00')})",
        "CREATE (:Ev {k: 'zoned_past', at: datetime('2000-01-01T00:00:00+05:00')})",
        "CREATE (:Ev {k: 'zoned_future', at: datetime('2999-01-01T00:00:00-05:00')})",
    ] {
        let parsed = parser::parse_cypher(q).unwrap();
        execute_mutable(
            &mut graph,
            &parsed,
            HashMap::new(),
            crate::graph::algorithms::Interrupt::default(),
        )
        .unwrap();
    }
    let names = |graph: &DirGraph, q: &str| -> Vec<String> {
        let parsed = parser::parse_cypher(q).unwrap();
        let params = HashMap::new();
        let mut out: Vec<String> = CypherExecutor::with_params(graph, &params, None)
            .execute(&parsed)
            .unwrap()
            .rows
            .into_iter()
            .map(|r| match &r[0] {
                Value::String(s) => s.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        out.sort();
        out
    };
    assert_eq!(
        names(&graph, "MATCH (n:Ev) WHERE n.at < datetime() RETURN n.k"),
        ["past", "zoned_past"]
    );
    assert_eq!(
        names(&graph, "MATCH (n:Ev) WHERE n.at > datetime() RETURN n.k"),
        ["future", "zoned_future"]
    );
    assert_eq!(
        names(&graph, "MATCH (n:Ev) WHERE datetime() > n.at RETURN n.k"),
        ["past", "zoned_past"]
    );
    assert_eq!(
        names(
            &graph,
            "MATCH (n:Ev) WHERE n.at = datetime('2000-01-01T00:00:00Z') RETURN n.k"
        ),
        ["past"]
    );
    // 2000-01-01T00:00+05:00 is 1999-12-31T19:00Z.
    assert_eq!(
        names(
            &graph,
            "MATCH (n:Ev) WHERE n.at = localdatetime('1999-12-31T19:00:00') RETURN n.k"
        ),
        ["zoned_past"]
    );
    // With equality and range indexes on the property the answers do not move.
    graph.create_index("Ev", "at");
    graph.create_range_index("Ev", "at");
    assert_eq!(
        names(&graph, "MATCH (n:Ev) WHERE n.at < datetime() RETURN n.k"),
        ["past", "zoned_past"]
    );
    assert_eq!(
        names(
            &graph,
            "MATCH (n:Ev) WHERE n.at = datetime('1999-12-31T20:00:00+01:00') RETURN n.k"
        ),
        ["zoned_past"]
    );
    assert_eq!(
        names(
            &graph,
            "MATCH (n:Ev) WHERE n.at = localdatetime('2000-01-01T00:00:00') RETURN n.k"
        ),
        ["past"]
    );
}

#[test]
fn arithmetic_moves_the_wall_clock_then_the_instant() {
    check(&[
        (
            "datetime('2015-07-21T21:40:32+01:00') + duration({hours: 5, minutes: 30})",
            at("2015-07-22T03:10:32+01:00"),
        ),
        (
            "duration({days: 1}) + datetime('2015-07-21T21:40:32-02:00')",
            at("2015-07-22T21:40:32-02:00"),
        ),
        // Months clamp to the end of a shorter month, on the wall clock.
        (
            "datetime('2015-01-31T23:30+05:00') + duration({months: 1})",
            at("2015-02-28T23:30:00+05:00"),
        ),
        (
            "datetime('2015-03-31T01:00+05:00') - duration({months: 1})",
            at("2015-02-28T01:00:00+05:00"),
        ),
        (
            "datetime('2015-07-21T21:40:32Z') + duration({nanoseconds: 7})",
            at("2015-07-21T21:40:32.000000007Z"),
        ),
        (
            "datetime('2015-07-21T21:40:32Z') - duration({seconds: 2, nanoseconds: 500000000})",
            at("2015-07-21T21:40:29.5Z"),
        ),
        (
            "datetime('2015-07-21T21:40Z') + 2",
            at("2015-07-23T21:40:00Z"),
        ),
        (
            "datetime('2015-07-21T21:40Z') - 1",
            at("2015-07-20T21:40:00Z"),
        ),
        ("datetime('2015-07-21T21:40Z') + null", Value::Null),
        // Two instants differ by the time between them, whatever their offsets.
        (
            "datetime('2015-07-21T22:40+01:00') - datetime('2015-07-21T20:40Z')",
            Value::Duration {
                months: 0,
                days: 0,
                seconds: 3600,
                nanos: 0,
            },
        ),
        (
            "datetime('2015-07-21T21:40:01.5+01:00') - datetime('2015-07-21T20:40Z')",
            Value::Duration {
                months: 0,
                days: 0,
                seconds: 1,
                nanos: 500_000_000,
            },
        ),
        // Against a local datetime, which is read as UTC.
        (
            "datetime('2015-07-21T22:40+01:00') - localdatetime('2015-07-21T20:00')",
            Value::Duration {
                months: 0,
                days: 0,
                seconds: 6000,
                nanos: 0,
            },
        ),
        (
            "localdatetime('2015-07-21T21:00') - datetime('2015-07-21T22:00+01:00')",
            Value::Duration {
                months: 0,
                days: 0,
                seconds: 0,
                nanos: 0,
            },
        ),
    ]);
}

#[test]
fn duration_between_counts_days_and_the_remainder() {
    let span = |days: i32, seconds: i64, nanos: i32| Value::Duration {
        months: 0,
        days,
        seconds,
        nanos,
    };
    check(&[
        (
            "duration.between(datetime('2015-07-21T21:40:32+01:00'), datetime('2015-07-23T19:40:32.5Z'))",
            span(1, 82_800, 500_000_000),
        ),
        (
            "duration.between(datetime('2015-07-23T00:00Z'), datetime('2015-07-21T00:00+02:00'))",
            span(-2, -7_200, 0),
        ),
        // A local side drops the zoned side's offset: the wall clocks are compared.
        (
            "duration.between(localdatetime('2015-07-21T10:00'), datetime('2015-07-21T12:00+05:00'))",
            span(0, 7200, 0),
        ),
        (
            "duration.between(date('2015-07-21'), datetime('2015-07-22T01:00+05:00'))",
            span(1, 3600, 0),
        ),
        ("duration.between(datetime('2015-07-21T10:00Z'), null)", Value::Null),
    ]);
    check(&[(
        "date_diff(datetime('2015-07-23T00:30+05:00'), date('2015-07-21'))",
        int(2),
    )]);
}

#[test]
fn truncation_rounds_the_wall_clock_and_keeps_the_offset() {
    check(&[
        (
            "datetime.truncate('hour', datetime('2015-07-21T21:40:32.142+01:00'))",
            at("2015-07-21T21:00:00+01:00"),
        ),
        (
            "datetime.truncate('day', datetime('2015-07-21T23:40:32-05:00'))",
            at("2015-07-21T00:00:00-05:00"),
        ),
        (
            "datetime.truncate('month', datetime('2015-07-21T01:40:32+05:00'))",
            at("2015-07-01T00:00:00+05:00"),
        ),
        (
            "datetime.truncate('millisecond', datetime('2015-07-21T21:40:32.142857123Z'))",
            at("2015-07-21T21:40:32.142Z"),
        ),
        (
            "datetime.truncate('hour', datetime('2015-07-21T21:40:32+01:00'), {timezone: '+03:00'})",
            at("2015-07-21T21:00:00+03:00"),
        ),
        // A local value truncates at UTC.
        (
            "datetime.truncate('day', localdatetime('2015-07-21T21:40:32'))",
            at("2015-07-21T00:00:00Z"),
        ),
        (
            "localdatetime.truncate('day', datetime('2015-07-21T21:40:32+05:00'))",
            local(2015, 7, 21, 0, 0, 0),
        ),
        (
            "date.truncate('month', datetime('2015-07-21T01:40:32+05:00'))",
            Value::DateTime(chrono::NaiveDate::from_ymd_opt(2015, 7, 1).unwrap()),
        ),
    ]);
}

#[test]
fn grouping_distinct_and_ordering_use_the_instant() {
    let rows = |q: &str| -> Vec<Value> {
        let graph = DirGraph::new();
        let parsed = parser::parse_cypher(q).unwrap();
        let params = HashMap::new();
        CypherExecutor::with_params(&graph, &params, None)
            .execute(&parsed)
            .unwrap()
            .rows
            .into_iter()
            .map(|mut r| r.remove(0))
            .collect()
    };
    let dbg = |v: Vec<Value>| format!("{v:?}");
    // DISTINCT merges the instant and keeps the first offset.
    assert_eq!(
        dbg(rows(
            "UNWIND [datetime('2015-07-21T21:40+01:00'), datetime('2015-07-21T20:40Z'), datetime('2015-07-21T09:00Z')] AS t RETURN DISTINCT t"
        )),
        dbg(vec![at("2015-07-21T21:40:00+01:00"), at("2015-07-21T09:00:00Z")])
    );
    // ORDER BY the instant: 08:30Z < 10:00+01:00 (09:00Z)? No: 10:00+01:00 is
    // 09:00Z, which is after 08:30Z and before 09:45Z.
    assert_eq!(
        dbg(rows(
            "UNWIND [datetime('2015-07-21T09:45Z'), datetime('2015-07-21T10:00+01:00'), datetime('2015-07-21T08:30Z')] AS t RETURN t ORDER BY t"
        )),
        dbg(vec![
            at("2015-07-21T08:30:00Z"),
            at("2015-07-21T10:00:00+01:00"),
            at("2015-07-21T09:45:00Z"),
        ])
    );
    assert_eq!(
        rows("UNWIND [datetime('2015-07-21T10:00+01:00'), datetime('2015-07-21T09:00Z'), datetime('2015-07-21T09:30Z')] AS t WITH t, count(*) AS n RETURN n ORDER BY n DESC LIMIT 1"),
        vec![int(2)]
    );
    // A column mixing the local, date and zoned kinds orders chronologically.
    assert_eq!(
        dbg(rows(
            "UNWIND [datetime('2015-07-21T12:00Z'), date('2015-07-21'), localdatetime('2015-07-21T06:00')] AS t RETURN t ORDER BY t"
        )),
        dbg(vec![
            Value::DateTime(chrono::NaiveDate::from_ymd_opt(2015, 7, 21).unwrap()),
            local(2015, 7, 21, 6, 0, 0),
            at("2015-07-21T12:00:00Z"),
        ])
    );
    assert_eq!(
        dbg(rows(
            "UNWIND [datetime('2015-07-21T09:00Z'), datetime('2015-07-21T08:30+01:00')] AS t RETURN min(t)"
        )),
        dbg(vec![at("2015-07-21T08:30:00+01:00")])
    );
}

#[test]
fn a_declared_zoned_datetime_property_accepts_only_zoned_values() {
    let mut graph = DirGraph::new();
    let run_mut = |graph: &mut DirGraph, q: &str| -> Result<(), String> {
        let parsed = parser::parse_cypher(q).unwrap();
        execute_mutable(
            graph,
            &parsed,
            HashMap::new(),
            crate::graph::algorithms::Interrupt::default(),
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    };
    run_mut(
        &mut graph,
        "CREATE CONSTRAINT FOR (p:Ev) REQUIRE p.at IS :: ZONED DATETIME",
    )
    .unwrap();
    run_mut(
        &mut graph,
        "CREATE (:Ev {at: datetime('2015-07-21T20:40+01:00')})",
    )
    .unwrap();
    for q in [
        "CREATE (:Ev {at: localdatetime('2015-07-21T20:40')})",
        "CREATE (:Ev {at: date('2015-07-21')})",
        "CREATE (:Ev {at: '2015-07-21T20:40Z'})",
    ] {
        let err = run_mut(&mut graph, q).expect_err(q);
        assert!(err.contains("ZONED DATETIME"), "{q}: {err}");
    }
}
