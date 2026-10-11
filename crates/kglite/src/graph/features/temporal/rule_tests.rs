//! The valid-time comparison rule at its source: which values are instants and
//! bounds, what each reads as, and the refusal text for the rest.

use super::eval::{parse_bounds, parse_instant, Instant, TemporalError};
use crate::datatypes::values::Value;
use chrono::{NaiveDate, NaiveDateTime};

fn stamp(text: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").unwrap()
}

#[test]
fn each_accepted_kind_reads_as_a_point_on_the_utc_timeline() {
    let day = NaiveDate::from_ymd_opt(2020, 3, 1).unwrap();
    let zoned = Value::ZonedDateTime {
        utc: stamp("2020-03-01T08:00:00"),
        offset_secs: 7200,
        zone: None,
    };
    assert_eq!(parse_instant(&Value::DateTime(day)), Ok(Instant::Date(day)));
    assert_eq!(
        parse_instant(&Value::Timestamp(stamp("2020-03-01T10:00:00"))),
        Ok(Instant::Timestamp(stamp("2020-03-01T10:00:00")))
    );
    // The wall clock (10:00+02:00) is display; the instant is 08:00Z.
    assert_eq!(
        parse_instant(&zoned),
        Ok(Instant::Timestamp(stamp("2020-03-01T08:00:00")))
    );
    assert_eq!(
        parse_instant(&Value::String("2020-03-01T10:00:00+02:00".into())),
        Ok(Instant::Timestamp(stamp("2020-03-01T08:00:00")))
    );
    assert_eq!(
        parse_instant(&Value::String("2020-03-01".into())),
        Ok(Instant::Date(day))
    );
}

#[test]
fn a_refused_kind_is_named_with_the_accepted_ones() {
    let refused = [
        (Value::LocalTime(36_000_000_000_000), "LOCAL TIME"),
        (
            Value::Time {
                nanos: 36_000_000_000_000,
                offset_secs: 0,
            },
            "ZONED TIME",
        ),
        (
            Value::Duration {
                months: 0,
                days: 1,
                seconds: 0,
                nanos: 0,
            },
            "DURATION",
        ),
        (Value::Int64(5), "INTEGER"),
        (Value::Boolean(true), "BOOLEAN"),
        (Value::Null, "NULL"),
        (Value::String("garbage".into()), "STRING"),
    ];
    for (value, kind) in refused {
        let message = parse_instant(&value).unwrap_err().to_string();
        assert!(
            message.contains(&format!(
                "({kind}) is not a date, a local datetime, a zoned datetime or an ISO date/datetime string"
            )),
            "{message}"
        );
        if value != Value::Null {
            let bound = parse_bounds(&Value::Null, &value).unwrap_err();
            assert!(matches!(bound, TemporalError::Bound { .. }));
            assert!(bound.to_string().starts_with("the to bound "), "{bound}");
        }
    }
}

#[test]
fn a_null_bound_is_open_but_a_null_instant_is_refused() {
    assert_eq!(parse_bounds(&Value::Null, &Value::Null), Ok((None, None)));
    assert!(parse_instant(&Value::Null).is_err());
}
