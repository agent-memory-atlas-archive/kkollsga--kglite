//! Time-of-day helpers shared by [`Value::LocalTime`](super::Value::LocalTime)
//! and [`Value::Time`](super::Value::Time): the nanosecond-of-day encoding, the
//! UTC-normalised comparison key, ISO 8601 parsing and the canonical text form.
//!
//! A time of day is a `u64` count of nanoseconds since local midnight, always
//! below [`NANOS_PER_DAY`] (no leap second). A zoned `Time` adds a UTC offset in
//! whole seconds, bounded by ±[`MAX_OFFSET_SECS`].
use chrono::{NaiveTime, Timelike};

pub const NANOS_PER_DAY: u64 = 86_400_000_000_000;
const NANOS_PER_SEC: i64 = 1_000_000_000;
/// Offsets beyond ±18:00 are not time-zone offsets (the Java and openCypher bound).
pub const MAX_OFFSET_SECS: i32 = 18 * 3600;

/// Whether `nanos` names a time of day.
#[inline]
pub fn is_valid_nanos(nanos: u64) -> bool {
    nanos < NANOS_PER_DAY
}

/// The comparison key of a zoned time: its clock reading moved to UTC. Not
/// wrapped into one day (`00:30+01:00` is `-1_800e9`, before `00:00Z`), which
/// is the order Java's `OffsetTime` and the openCypher TCK use. Two times with
/// equal keys are equal whatever their offsets.
#[inline]
pub fn utc_nanos(nanos: u64, offset_secs: i32) -> i64 {
    // nanos < 8.64e13 and |offset| < 1e5 s (< 1e14 ns): no i64 overflow.
    nanos as i64 - i64::from(offset_secs) * NANOS_PER_SEC
}

/// Nanoseconds since midnight of a `chrono` time (a leap second folds into the
/// last second of the day).
#[inline]
pub fn nanos_of(time: NaiveTime) -> u64 {
    u64::from(time.num_seconds_from_midnight().min(86_399)) * 1_000_000_000
        + u64::from(time.nanosecond().min(999_999_999))
}

/// The `chrono` time for a nanosecond-of-day, `None` past the end of the day.
pub fn naive_time(nanos: u64) -> Option<NaiveTime> {
    if !is_valid_nanos(nanos) {
        return None;
    }
    NaiveTime::from_num_seconds_from_midnight_opt(
        (nanos / 1_000_000_000) as u32,
        (nanos % 1_000_000_000) as u32,
    )
}

/// `nanos` moved by `seconds` + `sub_nanos` (a duration's clock part), wrapped
/// modulo one day: a time of day has no date to carry into.
pub fn shift_nanos(nanos: u64, seconds: i64, sub_nanos: i32) -> u64 {
    let day = i128::from(NANOS_PER_DAY);
    let delta = i128::from(seconds) * 1_000_000_000 + i128::from(sub_nanos);
    (i128::from(nanos) + delta).rem_euclid(day) as u64
}

/// The shortest ISO form of a clock reading: seconds are dropped when zero (with
/// no fraction), the fraction is printed in groups of three digits.
pub fn format_clock(nanos: u64) -> String {
    let secs = nanos / 1_000_000_000;
    let sub = (nanos % 1_000_000_000) as u32;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if sub == 0 && s == 0 {
        return format!("{h:02}:{m:02}");
    }
    let base = format!("{h:02}:{m:02}:{s:02}");
    if sub == 0 {
        base
    } else if sub.is_multiple_of(1_000_000) {
        format!("{base}.{:03}", sub / 1_000_000)
    } else if sub.is_multiple_of(1_000) {
        format!("{base}.{:06}", sub / 1_000)
    } else {
        format!("{base}.{sub:09}")
    }
}

/// `Z` for a zero offset, else `+HH:MM` (with `:SS` only when non-zero).
pub fn format_offset(offset_secs: i32) -> String {
    if offset_secs == 0 {
        return "Z".to_string();
    }
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let abs = offset_secs.unsigned_abs();
    let (h, m, s) = (abs / 3600, abs / 60 % 60, abs % 60);
    if s == 0 {
        format!("{sign}{h:02}:{m:02}")
    } else {
        format!("{sign}{h:02}:{m:02}:{s:02}")
    }
}

/// A zoned time as text: `21:40:32.142+01:00`, `10:00Z`.
pub fn format_time(nanos: u64, offset_secs: i32) -> String {
    format!("{}{}", format_clock(nanos), format_offset(offset_secs))
}

fn digits(s: &str) -> Option<u32> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().ok()
    } else {
        None
    }
}

/// Parse a clock reading: `HH`, `HHMM`, `HHMMSS[.f]`, `HH:MM`, `HH:MM:SS[.f]`
/// (fraction of 1-9 digits). No zone suffix is accepted.
pub fn parse_clock(text: &str) -> Option<NaiveTime> {
    let s = text.trim();
    if !s.is_ascii() {
        return None;
    }
    let (clock, fraction) = match s.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (s, None),
    };
    let (hour, minute, second) = if clock.contains(':') {
        let parts: Vec<&str> = clock.split(':').collect();
        if parts.iter().any(|p| p.len() != 2) {
            return None;
        }
        match parts.as_slice() {
            [h, m] => (digits(h)?, digits(m)?, 0),
            [h, m, sec] => (digits(h)?, digits(m)?, digits(sec)?),
            _ => return None,
        }
    } else {
        match clock.len() {
            2 => (digits(clock)?, 0, 0),
            4 => (digits(&clock[..2])?, digits(&clock[2..])?, 0),
            6 => (
                digits(&clock[..2])?,
                digits(&clock[2..4])?,
                digits(&clock[4..])?,
            ),
            _ => return None,
        }
    };
    let nanos = match fraction {
        None => 0,
        Some(f) if (1..=9).contains(&f.len()) && f.bytes().all(|b| b.is_ascii_digit()) => {
            // A fraction needs a seconds field to hang from.
            let has_seconds = if clock.contains(':') {
                clock.len() == 8
            } else {
                clock.len() == 6
            };
            if !has_seconds {
                return None;
            }
            digits(f)? * 10u32.pow(9 - f.len() as u32)
        }
        Some(_) => return None,
    };
    NaiveTime::from_hms_nano_opt(hour, minute, second, nanos)
}

/// Parse a UTC offset: `Z`, `±HH`, `±HHMM`, `±HH:MM`, `±HH:MM:SS`, `±HHMMSS`,
/// within ±18:00. `None` for anything else, including a named zone.
pub fn parse_offset(text: &str) -> Option<i32> {
    let s = text.trim();
    if s == "Z" || s == "z" {
        return Some(0);
    }
    let sign = match s.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let body = &s[1..];
    if !body.is_ascii() {
        return None;
    }
    let (h, m, sec) = if body.contains(':') {
        let parts: Vec<&str> = body.split(':').collect();
        if parts.iter().any(|p| p.len() != 2) {
            return None;
        }
        match parts.as_slice() {
            [h, m] => (digits(h)?, digits(m)?, 0),
            [h, m, sec] => (digits(h)?, digits(m)?, digits(sec)?),
            _ => return None,
        }
    } else {
        match body.len() {
            2 => (digits(body)?, 0, 0),
            4 => (digits(&body[..2])?, digits(&body[2..])?, 0),
            6 => (
                digits(&body[..2])?,
                digits(&body[2..4])?,
                digits(&body[4..])?,
            ),
            _ => return None,
        }
    };
    if m > 59 || sec > 59 {
        return None;
    }
    let total = i32::try_from(h * 3600 + m * 60 + sec).ok()?;
    (total <= MAX_OFFSET_SECS).then_some(sign * total)
}

/// Why a time-of-day text did not parse.
#[derive(Debug, PartialEq, Eq)]
pub enum TimeParseError {
    /// A `[Region/City]` suffix: a time of day has no date to pick the zone's
    /// offset by, so it takes numeric offsets only.
    NamedZone,
    /// Not a clock reading, or an offset that is not one.
    Malformed,
}

/// Split `text` into a clock and its optional offset suffix, and parse both:
/// `21:40:32.142`, `21:40:32Z`, `21:40+01:00`, `2140-0500`. The offset is
/// `None` when the text carries none.
pub fn parse_time(text: &str) -> Result<(u64, Option<i32>), TimeParseError> {
    let s = text.trim();
    if s.contains('[') {
        return Err(TimeParseError::NamedZone);
    }
    let (clock, offset) = match s.strip_suffix(['Z', 'z']) {
        Some(clock) => (clock, Some(0)),
        None => match s.find(['+', '-']) {
            Some(at) => (
                &s[..at],
                Some(parse_offset(&s[at..]).ok_or(TimeParseError::Malformed)?),
            ),
            None => (s, None),
        },
    };
    let time = parse_clock(clock).ok_or(TimeParseError::Malformed)?;
    Ok((nanos_of(time), offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_forms_round_trip_through_the_shortest_text() {
        for (text, rendered) in [
            ("21:40:32.142", "21:40:32.142"),
            ("214032.142", "21:40:32.142"),
            ("21:40", "21:40"),
            ("21", "21:00"),
            ("12:31:14.000000003", "12:31:14.000000003"),
            ("12:31:14.645876", "12:31:14.645876"),
        ] {
            let nanos = nanos_of(parse_clock(text).unwrap());
            assert_eq!(format_clock(nanos), rendered, "{text}");
        }
    }

    #[test]
    fn offsets_parse_and_print_canonically() {
        for (text, secs, printed) in [
            ("Z", 0, "Z"),
            ("+01", 3600, "+01:00"),
            ("+0130", 5400, "+01:30"),
            ("-05:00", -18000, "-05:00"),
            ("+18:00", 64800, "+18:00"),
            ("+01:00:30", 3630, "+01:00:30"),
        ] {
            assert_eq!(parse_offset(text), Some(secs), "{text}");
            assert_eq!(format_offset(secs), printed, "{text}");
        }
        for bad in ["+19:00", "+01:60", "01:00", "+1", "", "UTC", "+01:0"] {
            assert_eq!(parse_offset(bad), None, "{bad}");
        }
    }

    #[test]
    fn time_text_carries_an_optional_offset_and_refuses_a_named_zone() {
        assert_eq!(parse_time("10:00").unwrap(), (36_000_000_000_000, None));
        assert_eq!(
            parse_time("10:00:00.5+01:00").unwrap(),
            (36_000_500_000_000, Some(3600))
        );
        assert_eq!(parse_time("10Z").unwrap().1, Some(0));
        assert_eq!(parse_time("1000-0500").unwrap().1, Some(-18000));
        assert_eq!(
            parse_time("10:00[Europe/Oslo]"),
            Err(TimeParseError::NamedZone)
        );
        assert_eq!(parse_time("10:00+25:00"), Err(TimeParseError::Malformed));
        assert_eq!(parse_time("x"), Err(TimeParseError::Malformed));
    }

    #[test]
    fn shifting_wraps_modulo_one_day() {
        assert_eq!(shift_nanos(0, -1, 0), NANOS_PER_DAY - 1_000_000_000);
        assert_eq!(shift_nanos(NANOS_PER_DAY - 1, 0, 1), 0);
        assert_eq!(shift_nanos(5, 86_400 * 3, 0), 5);
    }

    #[test]
    fn utc_key_is_not_wrapped() {
        // 10:00+01:00 is 09:00Z; 09:35:14Z is later.
        let a = utc_nanos(36_000_000_000_000, 3600);
        let b = utc_nanos(34_514_000_000_000, 0);
        assert_eq!(a, 32_400_000_000_000);
        assert!(a < b);
        assert!(utc_nanos(1_800_000_000_000, 3600) < 0);
    }
}
