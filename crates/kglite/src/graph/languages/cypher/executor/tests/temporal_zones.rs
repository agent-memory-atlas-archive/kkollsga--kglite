//! Absolute goldens for named IANA zones on `ZonedDateTime`. Every expectation
//! is derived by hand from the published rules: Europe/Oslo moves to +02:00 at
//! 2026-03-29 01:00Z (02:00 -> 03:00 local) and back to +01:00 at 2026-10-25
//! 01:00Z (03:00 -> 02:00); Australia/Sydney moves back to +10:00 at
//! 2026-04-04 16:00Z (03:00 -> 02:00 local) and to +11:00 at 2026-10-03 16:00Z
//! (02:00 -> 03:00 local).

use super::temporal_functions::{cell, run};
use super::*;

fn s(text: &str) -> Value {
    Value::String(text.to_string())
}

fn string_of(expr: &str) -> Value {
    cell(&format!("RETURN toString({expr})"))
}

#[cfg(feature = "iana-zones")]
mod with_zones {
    use super::*;

    fn check(cases: &[(&str, &str)]) {
        for (expr, expected) in cases {
            assert_eq!(string_of(expr), s(expected), "{expr}");
        }
    }

    #[test]
    fn a_named_zone_places_the_reading_by_its_rules() {
        check(&[
            // Winter and summer offsets.
            (
                "datetime('2026-01-15T12:00[Europe/Oslo]')",
                "2026-01-15T12:00+01:00[Europe/Oslo]",
            ),
            (
                "datetime('2026-07-15T12:00[Europe/Oslo]')",
                "2026-07-15T12:00+02:00[Europe/Oslo]",
            ),
            // The map form names the zone the same way.
            (
                "datetime({year: 2026, month: 7, day: 15, hour: 12, timezone: 'Europe/Oslo'})",
                "2026-07-15T12:00+02:00[Europe/Oslo]",
            ),
            // An explicit offset fixes the instant; the zone is the view.
            (
                "datetime('2026-07-15T10:00Z[Europe/Oslo]')",
                "2026-07-15T12:00+02:00[Europe/Oslo]",
            ),
            (
                "datetime('2026-07-15T12:00+02:00[Europe/Oslo]')",
                "2026-07-15T12:00+02:00[Europe/Oslo]",
            ),
            // UTC prints as Z with its name.
            (
                "datetime({year: 2026, timezone: 'UTC'})",
                "2026-01-01T00:00Z[UTC]",
            ),
        ]);
    }

    #[test]
    fn spring_forward_shifts_a_skipped_reading_by_the_gap() {
        check(&[
            // 02:30 on 2026-03-29 does not exist in Oslo; it becomes 03:30+02:00.
            (
                "datetime('2026-03-29T02:30[Europe/Oslo]')",
                "2026-03-29T03:30+02:00[Europe/Oslo]",
            ),
            (
                "datetime({year: 2026, month: 3, day: 29, hour: 2, minute: 30, timezone: 'Europe/Oslo'})",
                "2026-03-29T03:30+02:00[Europe/Oslo]",
            ),
            // The edges of the gap are real: 01:59 is +01:00, 03:00 is +02:00.
            (
                "datetime('2026-03-29T01:59[Europe/Oslo]')",
                "2026-03-29T01:59+01:00[Europe/Oslo]",
            ),
            (
                "datetime('2026-03-29T03:00[Europe/Oslo]')",
                "2026-03-29T03:00+02:00[Europe/Oslo]",
            ),
            // Southern hemisphere: Sydney skips 02:00-03:00 on 2026-10-04.
            (
                "datetime('2026-10-04T02:30[Australia/Sydney]')",
                "2026-10-04T03:30+11:00[Australia/Sydney]",
            ),
        ]);
        // The shifted reading is the instant 01:30Z = 1_774_747_800 s.
        assert_eq!(
            cell("RETURN datetime('2026-03-29T02:30[Europe/Oslo]').epochSeconds"),
            Value::Int64(1_774_747_800)
        );
        assert_eq!(
            cell("RETURN datetime('2026-10-04T02:30[Australia/Sydney]').epochSeconds"),
            Value::Int64(1_791_045_000)
        );
    }

    #[test]
    fn fall_back_takes_the_earlier_offset_unless_the_text_says_otherwise() {
        check(&[
            // 02:30 on 2026-10-25 happens twice in Oslo; the earlier is +02:00.
            (
                "datetime('2026-10-25T02:30[Europe/Oslo]')",
                "2026-10-25T02:30+02:00[Europe/Oslo]",
            ),
            // An explicit offset picks the later occurrence (01:30Z).
            (
                "datetime('2026-10-25T02:30+01:00[Europe/Oslo]')",
                "2026-10-25T02:30+01:00[Europe/Oslo]",
            ),
            // Sydney repeats 02:30 on 2026-04-05; the earlier is +11:00.
            (
                "datetime('2026-04-05T02:30[Australia/Sydney]')",
                "2026-04-05T02:30+11:00[Australia/Sydney]",
            ),
        ]);
        assert_eq!(
            cell("RETURN datetime('2026-10-25T02:30[Europe/Oslo]').epochSeconds"),
            Value::Int64(1_792_888_200)
        );
        assert_eq!(
            cell("RETURN datetime('2026-10-25T02:30+01:00[Europe/Oslo]').epochSeconds"),
            Value::Int64(1_792_891_800)
        );
        assert_eq!(
            cell("RETURN datetime('2026-04-05T02:30[Australia/Sydney]').epochSeconds"),
            Value::Int64(1_775_316_600)
        );
    }

    #[test]
    fn calendar_parts_follow_the_wall_clock_and_clock_parts_the_instant() {
        check(&[
            // +P1D keeps 12:00 across spring-forward (a 23 hour step);
            // +PT24H is 24 elapsed hours (13:00 local).
            (
                "datetime('2026-03-28T12:00[Europe/Oslo]') + duration('P1D')",
                "2026-03-29T12:00+02:00[Europe/Oslo]",
            ),
            (
                "datetime('2026-03-28T12:00[Europe/Oslo]') + duration('PT24H')",
                "2026-03-29T13:00+02:00[Europe/Oslo]",
            ),
            // Fall back: +P1D is a 25 hour step, +PT24H is 11:00 local.
            (
                "datetime('2026-10-24T12:00[Europe/Oslo]') + duration('P1D')",
                "2026-10-25T12:00+01:00[Europe/Oslo]",
            ),
            (
                "datetime('2026-10-24T12:00[Europe/Oslo]') + duration('PT24H')",
                "2026-10-25T11:00+01:00[Europe/Oslo]",
            ),
            // A calendar result inside the gap moves forward by the gap.
            (
                "datetime('2026-03-28T02:30[Europe/Oslo]') + duration('P1D')",
                "2026-03-29T03:30+02:00[Europe/Oslo]",
            ),
            // One hour on the instant crosses the transition.
            (
                "datetime('2026-03-29T01:30[Europe/Oslo]') + duration('PT1H')",
                "2026-03-29T03:30+02:00[Europe/Oslo]",
            ),
            // Months move the calendar, then the zone resolves the offset.
            (
                "datetime('2026-02-15T12:00[Europe/Oslo]') + duration('P5M')",
                "2026-07-15T12:00+02:00[Europe/Oslo]",
            ),
            // Subtraction mirrors it.
            (
                "datetime('2026-03-29T12:00[Europe/Oslo]') - duration('P1D')",
                "2026-03-28T12:00+01:00[Europe/Oslo]",
            ),
            // A later repeated reading keeps its offset under calendar steps.
            (
                "datetime('2026-10-25T02:30+01:00[Europe/Oslo]') + duration('P0D')",
                "2026-10-25T02:30+01:00[Europe/Oslo]",
            ),
        ]);
        // Elapsed time between the two is the instants' difference, not the clock's.
        assert_eq!(
            string_of(
                "duration.between(datetime('2026-03-28T12:00[Europe/Oslo]'), datetime('2026-03-29T12:00[Europe/Oslo]'))"
            ),
            s("PT23H")
        );
    }

    #[test]
    fn accessors_report_the_name_the_offset_and_the_wall_clock() {
        for (expr, expected) in [
            (
                "datetime('2026-07-15T12:00[Europe/Oslo]').timezone",
                s("Europe/Oslo"),
            ),
            (
                "datetime('2026-07-15T12:00[Europe/Oslo]').offset",
                s("+02:00"),
            ),
            (
                "datetime('2026-07-15T12:00[Europe/Oslo]').offsetSeconds",
                Value::Int64(7200),
            ),
            (
                "datetime('2026-07-15T12:00[Europe/Oslo]').hour",
                Value::Int64(12),
            ),
            // A fixed offset has no name: `timezone` is its offset text.
            ("datetime('2026-07-15T12:00+02:00').timezone", s("+02:00")),
        ] {
            assert_eq!(cell(&format!("RETURN {expr}")), expected, "{expr}");
        }
    }

    #[test]
    fn identity_is_the_instant_not_the_zone() {
        for (expr, expected) in [
            // Same instant in two zones, and in a zone and an offset.
            (
                "datetime('2026-03-29T03:30[Europe/Oslo]') = datetime('2026-03-29T01:30Z')",
                true,
            ),
            (
                "datetime('2026-07-15T12:00[Europe/Oslo]') = datetime('2026-07-15T06:00[America/New_York]')",
                true,
            ),
            (
                "datetime('2026-07-15T12:00[Europe/Oslo]') < datetime('2026-07-15T12:00[America/New_York]')",
                true,
            ),
        ] {
            assert_eq!(cell(&format!("RETURN {expr}")), Value::Boolean(expected), "{expr}");
        }
        // DISTINCT keeps one value per instant.
        assert_eq!(
            cell("UNWIND [datetime('2026-07-15T12:00[Europe/Oslo]'), datetime('2026-07-15T10:00Z')] AS d RETURN count(DISTINCT d)"),
            Value::Int64(1)
        );
    }

    #[test]
    fn a_zoned_base_keeps_its_zone_through_the_map_form() {
        check(&[
            // Same instant in another zone, then the clock override on that reading.
            (
                "datetime({datetime: datetime('1984-10-11T12:00[Europe/Stockholm]'), timezone: '+05:00'})",
                "1984-10-11T16:00+05:00",
            ),
            (
                "datetime({datetime: datetime('1984-10-11T12:00[Europe/Stockholm]'), day: 28, second: 42})",
                "1984-10-28T12:00:42+01:00[Europe/Stockholm]",
            ),
            (
                "datetime({datetime: datetime('1984-10-11T12:00[Europe/Stockholm]'), day: 28, second: 42, timezone: 'Pacific/Honolulu'})",
                "1984-10-28T01:00:42-10:00[Pacific/Honolulu]",
            ),
            // A local base takes the zone as written.
            (
                "datetime({datetime: localdatetime('2026-07-15T12:00'), timezone: 'Europe/Oslo'})",
                "2026-07-15T12:00+02:00[Europe/Oslo]",
            ),
            // Truncation keeps the zone, and re-places under a new one.
            (
                "datetime.truncate('day', datetime('2026-07-15T12:34[Europe/Oslo]'))",
                "2026-07-15T00:00+02:00[Europe/Oslo]",
            ),
            (
                "datetime.truncate('hour', datetime('2026-07-15T12:34+05:00'), {timezone: 'Europe/Oslo'})",
                "2026-07-15T12:00+02:00[Europe/Oslo]",
            ),
        ]);
    }

    #[test]
    fn a_time_of_day_takes_the_offset_of_a_zoned_value_but_no_named_zone() {
        for (expr, expected) in [
            (
                "time(datetime('2026-07-15T12:00[Europe/Oslo]'))",
                "12:00+02:00",
            ),
            (
                "time(datetime('2026-01-15T12:00[Europe/Oslo]'))",
                "12:00+01:00",
            ),
            (
                "time({time: datetime('2026-07-15T12:00[Europe/Oslo]'), timezone: '+05:00'})",
                "15:00+05:00",
            ),
            (
                "localtime(datetime('2026-07-15T12:00[Europe/Oslo]'))",
                "12:00",
            ),
        ] {
            assert_eq!(string_of(expr), s(expected), "{expr}");
        }
        for query in [
            "RETURN time('10:00[Europe/Oslo]')",
            "RETURN time({hour: 10, timezone: 'Europe/Oslo'})",
        ] {
            let err = run(query).unwrap_err();
            assert!(err.contains("no date"), "{query}: {err}");
        }
    }

    #[test]
    fn unknown_zones_are_refused_by_name() {
        for query in [
            "RETURN datetime('2026-07-15T12:00[Mars/Base]')",
            "RETURN datetime({year: 2026, timezone: 'Mars/Base'})",
        ] {
            let err = run(query).unwrap_err();
            assert!(err.contains("Mars/Base"), "{query}: {err}");
        }
    }
}

#[cfg(not(feature = "iana-zones"))]
#[test]
fn without_the_feature_a_named_zone_is_refused_naming_it() {
    for query in [
        "RETURN datetime('2026-07-15T12:00[Europe/Oslo]')",
        "RETURN datetime({year: 2026, timezone: 'Europe/Oslo'})",
    ] {
        let err = run(query).unwrap_err();
        assert!(err.contains("iana-zones"), "{query}: {err}");
    }
}
