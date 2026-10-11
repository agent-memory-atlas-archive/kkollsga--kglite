//! Helpers for [`Value::ZonedDateTime`](super::Value::ZonedDateTime): the
//! instant is a UTC-naive `NaiveDateTime` (the engine's convention, shared with
//! `Value::Timestamp`), the offset is whole seconds within
//! ±[`MAX_OFFSET_SECS`](super::time_of_day::MAX_OFFSET_SECS), and the wall
//! clock is derived as `utc + offset`.
use super::time_of_day::TimeParseError;
use super::time_of_day::{format_clock, format_offset, naive_time, nanos_of, parse_time};
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeDelta};

/// The wall-clock reading of an instant at `offset_secs`. Falls back to the
/// instant itself in the last day before the representable range ends, where
/// the shifted reading would not exist.
pub fn wall_clock(utc: NaiveDateTime, offset_secs: i32) -> NaiveDateTime {
    TimeDelta::try_seconds(i64::from(offset_secs))
        .and_then(|delta| utc.checked_add_signed(delta))
        .unwrap_or(utc)
}

/// The instant a wall-clock reading names at `offset_secs`; `None` when it
/// falls outside the representable range.
pub fn utc_from_wall(wall: NaiveDateTime, offset_secs: i32) -> Option<NaiveDateTime> {
    TimeDelta::try_seconds(i64::from(offset_secs)).and_then(|delta| wall.checked_sub_signed(delta))
}

/// The instant at `secs` + `nanos` since the Unix epoch, `None` out of range.
pub fn from_epoch(secs: i64, nanos: u32) -> Option<NaiveDateTime> {
    DateTime::from_timestamp(secs, nanos).map(|dt| dt.naive_utc())
}

/// The instant as epoch seconds and the sub-second nanoseconds (`0..1e9`).
pub fn epoch_parts(utc: NaiveDateTime) -> (i64, u32) {
    let utc = utc.and_utc();
    (utc.timestamp(), utc.timestamp_subsec_nanos())
}

/// ISO 8601 text of an instant at its offset: `2015-07-21T21:40:32.142+01:00`,
/// `1984-10-11T12:31Z`. The clock follows [`format_clock`] (zero seconds are
/// dropped, the fraction comes in groups of three digits).
pub fn format_zoned(utc: NaiveDateTime, offset_secs: i32) -> String {
    let wall = wall_clock(utc, offset_secs);
    format!(
        "{}T{}{}",
        wall.date(),
        format_clock(nanos_of(wall.time())),
        format_offset(offset_secs)
    )
}

/// Why a datetime text did not parse.
#[derive(Debug, PartialEq, Eq)]
pub enum ZonedParseError {
    /// A `[Region/City]` suffix: zone rules are not available yet.
    NamedZone,
    /// Not a date, a clock reading, or an offset.
    Malformed,
}

/// The plain calendar spellings: `YYYY-MM-DD` and `YYYYMMDD`.
pub fn parse_plain_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(text, "%Y%m%d"))
        .ok()
}

/// Parse `<date>[T<clock>[<offset>]]` into the wall-clock reading and the offset
/// the text carried (`None` when it carried none). A bare date is midnight. The
/// date half is read by `parse_date`, so a caller can accept more spellings
/// than [`parse_plain_date`].
pub fn parse_with_offset(
    text: &str,
    parse_date: impl Fn(&str) -> Option<NaiveDate>,
) -> Result<(NaiveDateTime, Option<i32>), ZonedParseError> {
    let s = text.trim();
    if s.contains('[') {
        return Err(ZonedParseError::NamedZone);
    }
    match s.split_once(['T', 't']) {
        Some((date, clock)) => {
            let date = parse_date(date).ok_or(ZonedParseError::Malformed)?;
            let (nanos, offset) = parse_time(clock).map_err(|e| match e {
                TimeParseError::NamedZone => ZonedParseError::NamedZone,
                TimeParseError::Malformed => ZonedParseError::Malformed,
            })?;
            let time = naive_time(nanos).ok_or(ZonedParseError::Malformed)?;
            Ok((date.and_time(time), offset))
        }
        None => {
            let date = parse_date(s).ok_or(ZonedParseError::Malformed)?;
            Ok((date.and_time(chrono::NaiveTime::MIN), None))
        }
    }
}

/// [`parse_with_offset`] with the plain date spellings, resolved to the
/// instant: a text with no offset is read as UTC.
pub fn parse_zoned(text: &str) -> Result<(NaiveDateTime, i32), ZonedParseError> {
    let (wall, offset) = parse_with_offset(text, parse_plain_date)?;
    let offset = offset.unwrap_or(0);
    let utc = utc_from_wall(wall, offset).ok_or(ZonedParseError::Malformed)?;
    Ok((utc, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64, nanos: u32) -> NaiveDateTime {
        from_epoch(secs, nanos).unwrap()
    }

    #[test]
    fn wall_clock_and_instant_invert() {
        let utc = at(1_437_511_232, 142_000_000);
        for off in [0, 3600, -18_000, 64_800, -64_800, 3630] {
            assert_eq!(utc_from_wall(wall_clock(utc, off), off), Some(utc));
        }
    }

    #[test]
    fn text_is_the_wall_clock_with_its_offset() {
        let utc = at(1_437_511_232, 142_000_000);
        assert_eq!(format_zoned(utc, 3600), "2015-07-21T21:40:32.142+01:00");
        assert_eq!(format_zoned(utc, 0), "2015-07-21T20:40:32.142Z");
        assert_eq!(format_zoned(at(1_437_511_200, 0), 0), "2015-07-21T20:40Z");
        assert_eq!(epoch_parts(utc), (1_437_511_232, 142_000_000));
        assert!(from_epoch(i64::MAX, 0).is_none());
    }

    #[test]
    fn text_round_trips_and_names_what_it_refuses() {
        for text in [
            "2015-07-21T21:40:32.142+01:00",
            "2015-07-21T20:40:32.142Z",
            "2015-07-21T20:40Z",
            "1999-12-31T23:59:59.000000001-05:30",
        ] {
            let (utc, off) = parse_zoned(text).unwrap();
            assert_eq!(format_zoned(utc, off), text);
        }
        // No offset reads as UTC; a bare date is midnight.
        assert_eq!(parse_zoned("2015-07-21T10:00").unwrap().1, 0);
        assert_eq!(
            format_zoned(parse_zoned("2015-07-21").unwrap().0, 0),
            "2015-07-21T00:00Z"
        );
        assert_eq!(parse_zoned("20150721T1000+0100").unwrap().1, 3600);
        assert_eq!(
            parse_zoned("2015-07-21T10:00[Europe/Oslo]"),
            Err(ZonedParseError::NamedZone)
        );
        for bad in ["x", "2015-07-21T25:00Z", "2015-07-21T10:00+19:00", ""] {
            assert_eq!(parse_zoned(bad), Err(ZonedParseError::Malformed), "{bad}");
        }
    }
}
