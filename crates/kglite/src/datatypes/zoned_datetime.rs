//! Helpers for [`Value::ZonedDateTime`](super::Value::ZonedDateTime): the
//! instant is a UTC-naive `NaiveDateTime` (the engine's convention, shared with
//! `Value::Timestamp`), the offset is whole seconds within
//! ±[`MAX_OFFSET_SECS`](super::time_of_day::MAX_OFFSET_SECS), and the wall
//! clock is derived as `utc + offset`. A value in a named zone also carries a
//! [`ZoneId`], whose offset at the instant is the stored offset; the text forms
//! here read and print the `[Region/City]` suffix.
use super::time_of_day::{format_clock, format_offset, naive_time, nanos_of, parse_time};
use super::time_zone::{ZoneId, IANA_ZONES};
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

/// ISO 8601 text of an instant at its offset, with the zone name appended when
/// it has one: `2015-07-21T21:40:32.142+02:00[Europe/Stockholm]`.
pub fn format_zoned_in(utc: NaiveDateTime, offset_secs: i32, zone: Option<ZoneId>) -> String {
    match zone {
        None => format_zoned(utc, offset_secs),
        Some(zone) => format!("{}[{}]", format_zoned(utc, offset_secs), zone.name()),
    }
}

/// Why a datetime text did not parse.
#[derive(Debug, PartialEq, Eq)]
pub enum ZonedParseError {
    /// Not a date, a clock reading, or an offset.
    Malformed,
    /// A `[Region/City]` suffix naming no zone in the database.
    UnknownZone(String),
    /// A `[Region/City]` suffix in a build without the `iana-zones` feature.
    ZonesUnavailable(String),
}

impl std::fmt::Display for ZonedParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ZonedParseError::Malformed => f.write_str("not an ISO-8601 datetime"),
            ZonedParseError::UnknownZone(name) => write!(f, "unknown time zone '{name}'"),
            ZonedParseError::ZonesUnavailable(name) => write!(
                f,
                "named time zone '{name}' needs the `iana-zones` feature, which this build omits; use a numeric offset such as +01:00"
            ),
        }
    }
}

/// The zone called `name`, or why there is none.
pub fn zone_by_name(name: &str) -> Result<ZoneId, ZonedParseError> {
    if !IANA_ZONES {
        return Err(ZonedParseError::ZonesUnavailable(name.to_string()));
    }
    ZoneId::from_name(name).ok_or_else(|| ZonedParseError::UnknownZone(name.to_string()))
}

/// Split a trailing `[Region/City]` off `text`: the text before it and the
/// name inside. `None` for a text with no bracket; a bracket that is not
/// closed at the very end is malformed.
pub fn split_zone_suffix(text: &str) -> Result<(&str, Option<&str>), ZonedParseError> {
    let Some(open) = text.find('[') else {
        return Ok((text, None));
    };
    let Some(name) = text[open + 1..].strip_suffix(']') else {
        return Err(ZonedParseError::Malformed);
    };
    if name.is_empty() || name.contains(['[', ']']) {
        return Err(ZonedParseError::Malformed);
    }
    Ok((&text[..open], Some(name)))
}

/// The plain calendar spellings: `YYYY-MM-DD` and `YYYYMMDD`.
pub fn parse_plain_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(text, "%Y%m%d"))
        .ok()
}

/// The wall-clock reading, offset and zone a datetime text carried.
#[derive(Debug, PartialEq, Eq)]
pub struct WallParts {
    pub wall: NaiveDateTime,
    /// `None` when the text carried no offset.
    pub offset_secs: Option<i32>,
    /// `None` when the text carried no `[zone]`.
    pub zone: Option<ZoneId>,
}

/// Parse `<date>[T<clock>[<offset>]][[Region/City]]` into the wall-clock
/// reading and what the text said about its offset and zone. A bare date is
/// midnight. The date half is read by `parse_date`, so a caller can accept more
/// spellings than [`parse_plain_date`].
pub fn parse_with_zone(
    text: &str,
    parse_date: impl Fn(&str) -> Option<NaiveDate>,
) -> Result<WallParts, ZonedParseError> {
    let (s, zone_name) = split_zone_suffix(text.trim())?;
    let zone = zone_name.map(zone_by_name).transpose()?;
    let (wall, offset_secs) = match s.split_once(['T', 't']) {
        Some((date, clock)) => {
            let date = parse_date(date).ok_or(ZonedParseError::Malformed)?;
            let (nanos, offset) = parse_time(clock).map_err(|_| ZonedParseError::Malformed)?;
            let time = naive_time(nanos).ok_or(ZonedParseError::Malformed)?;
            (date.and_time(time), offset)
        }
        None => {
            let date = parse_date(s).ok_or(ZonedParseError::Malformed)?;
            (date.and_time(chrono::NaiveTime::MIN), None)
        }
    };
    Ok(WallParts {
        wall,
        offset_secs,
        zone,
    })
}

/// An instant with its offset and, when it has one, its named zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZonedParts {
    pub utc: NaiveDateTime,
    pub offset_secs: i32,
    pub zone: Option<ZoneId>,
}

impl ZonedParts {
    /// As a [`Value::ZonedDateTime`](super::Value::ZonedDateTime).
    pub fn into_value(self) -> super::Value {
        super::Value::ZonedDateTime {
            utc: self.utc,
            offset_secs: self.offset_secs,
            zone: self.zone,
        }
    }
}

/// The instant a wall-clock reading names, given what is known about its
/// offset and zone. No zone: the offset (UTC when absent) is fixed. A zone and
/// an offset: the offset fixes the instant, which is then seen in the zone. A
/// zone alone: the zone's rules place the reading (a skipped reading moves
/// forward by the gap, a repeated one takes the earlier offset). `None` when
/// the instant is out of range.
pub fn resolve_wall(
    wall: NaiveDateTime,
    offset_secs: Option<i32>,
    zone: Option<ZoneId>,
) -> Option<ZonedParts> {
    match (zone, offset_secs) {
        (None, offset) => {
            let offset_secs = offset.unwrap_or(0);
            Some(ZonedParts {
                utc: utc_from_wall(wall, offset_secs)?,
                offset_secs,
                zone: None,
            })
        }
        (Some(zone), Some(offset)) => {
            let utc = utc_from_wall(wall, offset)?;
            Some(ZonedParts {
                utc,
                offset_secs: zone.offset_at(utc),
                zone: Some(zone),
            })
        }
        (Some(zone), None) => {
            let at = zone.resolve_local(wall, None)?;
            Some(ZonedParts {
                utc: at.utc,
                offset_secs: at.offset_secs,
                zone: Some(zone),
            })
        }
    }
}

/// [`parse_with_zone`] with the plain date spellings, resolved to the instant:
/// a text with neither offset nor zone is read as UTC.
pub fn parse_zoned(text: &str) -> Result<ZonedParts, ZonedParseError> {
    let parts = parse_with_zone(text, parse_plain_date)?;
    resolve_wall(parts.wall, parts.offset_secs, parts.zone).ok_or(ZonedParseError::Malformed)
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
            let p = parse_zoned(text).unwrap();
            assert_eq!(format_zoned_in(p.utc, p.offset_secs, p.zone), text);
        }
        // No offset reads as UTC; a bare date is midnight.
        assert_eq!(parse_zoned("2015-07-21T10:00").unwrap().offset_secs, 0);
        let midnight = parse_zoned("2015-07-21").unwrap();
        assert_eq!(format_zoned(midnight.utc, 0), "2015-07-21T00:00Z");
        assert_eq!(parse_zoned("20150721T1000+0100").unwrap().offset_secs, 3600);
        for bad in [
            "x",
            "2015-07-21T25:00Z",
            "2015-07-21T10:00+19:00",
            "",
            "2015-07-21T10:00[",
            "2015-07-21T10:00[]",
            "2015-07-21T10:00[Europe/Oslo]x",
        ] {
            assert_eq!(parse_zoned(bad), Err(ZonedParseError::Malformed), "{bad}");
        }
    }

    #[cfg(feature = "iana-zones")]
    #[test]
    fn named_zones_parse_resolve_and_print() {
        // Summer in Oslo is +02:00; the zone alone places the reading.
        let p = parse_zoned("2026-07-01T12:00[Europe/Oslo]").unwrap();
        assert_eq!(p.offset_secs, 7200);
        assert_eq!(
            format_zoned_in(p.utc, p.offset_secs, p.zone),
            "2026-07-01T12:00+02:00[Europe/Oslo]"
        );
        // An offset with the zone fixes the instant; the zone supplies the rest.
        let q = parse_zoned("2026-07-01T10:00Z[Europe/Oslo]").unwrap();
        assert_eq!(
            format_zoned_in(q.utc, q.offset_secs, q.zone),
            "2026-07-01T12:00+02:00[Europe/Oslo]"
        );
        assert_eq!(
            parse_zoned("2026-07-01T12:00[Mars/Base]"),
            Err(ZonedParseError::UnknownZone("Mars/Base".into()))
        );
    }

    #[cfg(not(feature = "iana-zones"))]
    #[test]
    fn named_zones_are_refused_naming_the_feature() {
        let err = parse_zoned("2026-07-01T12:00[Europe/Oslo]").unwrap_err();
        assert!(matches!(err, ZonedParseError::ZonesUnavailable(_)));
        assert!(err.to_string().contains("iana-zones"));
    }
}
