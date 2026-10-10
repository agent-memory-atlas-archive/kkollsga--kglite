//! ISO 8601 text forms of the zoneless temporal values: parsing the calendar
//! (`YYYY-MM-DD`, `YYYYMMDD`, ISO week `YYYY-Www-D`, ordinal `YYYY-DDD`) and
//! clock (`HH`, `HHMM`, `HH:MM:SS.fff`) spellings, and rendering a clock in
//! the canonical minimal form (`HH:MM`, `HH:MM:SS`, fraction in groups of
//! three digits). Pure functions over `chrono`, shared by the `date()`,
//! `localtime()` and `localdatetime()` constructors.
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

fn digits(s: &str) -> Option<u32> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().ok()
    } else {
        None
    }
}

/// The Monday that starts ISO week 1 of `year` (the week holding January 4th).
pub(super) fn iso_week_one_monday(year: i32) -> Option<NaiveDate> {
    let jan4 = NaiveDate::from_ymd_opt(year, 1, 4)?;
    jan4.checked_sub_signed(chrono::TimeDelta::days(
        jan4.weekday().num_days_from_monday() as i64,
    ))
}

/// The date of ISO weekday `day_of_week` (1 = Monday) of week `week` of the
/// ISO week-year `year`. A week past the year's last is refused.
pub(super) fn date_from_iso_week(
    year: i32,
    week: i64,
    day_of_week: i64,
) -> Result<NaiveDate, String> {
    if !(1..=7).contains(&day_of_week) {
        return Err(format!(
            "dayOfWeek must be between 1 and 7, got {day_of_week}"
        ));
    }
    let last_week = iso_week_count(year).ok_or_else(|| format!("year {year} out of range"))?;
    if week < 1 || week > last_week {
        return Err(format!(
            "week must be between 1 and {last_week} for {year}, got {week}"
        ));
    }
    iso_week_one_monday(year)
        .and_then(|monday| {
            monday.checked_add_signed(chrono::TimeDelta::days((week - 1) * 7 + day_of_week - 1))
        })
        .ok_or_else(|| format!("week date {year}-W{week}-{day_of_week} out of range"))
}

/// Number of ISO weeks (52 or 53) in the week-year `year`.
fn iso_week_count(year: i32) -> Option<i64> {
    let start = iso_week_one_monday(year)?;
    let next = iso_week_one_monday(year.checked_add(1)?)?;
    Some((next - start).num_days() / 7)
}

/// The date of day `ordinal` (1-based) of `year`.
pub(super) fn date_from_ordinal(year: i32, ordinal: i64) -> Result<NaiveDate, String> {
    u32::try_from(ordinal)
        .ok()
        .and_then(|day| NaiveDate::from_yo_opt(year, day))
        .ok_or_else(|| format!("ordinalDay {ordinal} is not a day of {year}"))
}

/// The date of day `day_of_quarter` (1-based) of quarter `quarter` of `year`.
pub(super) fn date_from_quarter(
    year: i32,
    quarter: i64,
    day_of_quarter: i64,
) -> Result<NaiveDate, String> {
    if !(1..=4).contains(&quarter) {
        return Err(format!("quarter must be between 1 and 4, got {quarter}"));
    }
    let first = NaiveDate::from_ymd_opt(year, ((quarter - 1) * 3 + 1) as u32, 1)
        .ok_or_else(|| format!("year {year} out of range"))?;
    let next_first = if quarter == 4 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, (quarter * 3 + 1) as u32, 1)
    };
    let length = next_first.map_or(92, |next| (next - first).num_days());
    if day_of_quarter < 1 || day_of_quarter > length {
        return Err(format!(
            "dayOfQuarter must be between 1 and {length} for quarter {quarter} of {year}, got {day_of_quarter}"
        ));
    }
    first
        .checked_add_signed(chrono::TimeDelta::days(day_of_quarter - 1))
        .ok_or_else(|| "date out of range".to_string())
}

/// 1-based day within the quarter that holds `date`.
pub(super) fn day_of_quarter(date: NaiveDate) -> i64 {
    let first_month = (date.month0() / 3) * 3 + 1;
    NaiveDate::from_ymd_opt(date.year(), first_month, 1)
        .map_or(1, |first| (date - first).num_days() + 1)
}

/// Parse a calendar date in any ISO 8601 spelling openCypher uses.
/// `None` when the text is none of them (the caller falls back to the looser
/// `YYYY-M-D` reader).
pub(super) fn parse_date_text(text: &str) -> Option<NaiveDate> {
    let s = text.trim();
    if !s.is_ascii() {
        return None;
    }
    if let Some(w) = s.find('W') {
        // YYYY-Www, YYYYWww, YYYY-Www-D, YYYYWwwD
        let year_part = s[..w].strip_suffix('-').unwrap_or(&s[..w]);
        if year_part.len() != 4 {
            return None;
        }
        let year = digits(year_part)? as i32;
        let rest = &s[w + 1..];
        let (week, dow) = match rest.len() {
            2 => (digits(rest)?, 1),
            4 if rest.as_bytes()[2] == b'-' => (digits(&rest[..2])?, digits(&rest[3..])?),
            3 => (digits(&rest[..2])?, digits(&rest[2..])?),
            _ => return None,
        };
        // `W` introduces a dash only when the year part carried one.
        let dashed_year = s[..w].ends_with('-');
        let dashed_day = rest.len() == 4;
        if rest.len() > 2 && dashed_year != dashed_day {
            return None;
        }
        return date_from_iso_week(year, week as i64, dow as i64).ok();
    }
    let compact = s.len() >= 4 && !s.contains('-') && digits(s).is_some();
    if compact {
        return match s.len() {
            4 => NaiveDate::from_ymd_opt(digits(s)? as i32, 1, 1),
            6 => NaiveDate::from_ymd_opt(digits(&s[..4])? as i32, digits(&s[4..])?, 1),
            7 => NaiveDate::from_yo_opt(digits(&s[..4])? as i32, digits(&s[4..])?),
            8 => NaiveDate::from_ymd_opt(
                digits(&s[..4])? as i32,
                digits(&s[4..6])?,
                digits(&s[6..])?,
            ),
            _ => None,
        };
    }
    let parts: Vec<&str> = s.split('-').collect();
    match parts.as_slice() {
        [y, ordinal] if y.len() == 4 && ordinal.len() == 3 => {
            NaiveDate::from_yo_opt(digits(y)? as i32, digits(ordinal)?)
        }
        [y, m] if y.len() == 4 && m.len() == 2 => {
            NaiveDate::from_ymd_opt(digits(y)? as i32, digits(m)?, 1)
        }
        [y, m, d] if y.len() == 4 && m.len() == 2 && d.len() == 2 => {
            NaiveDate::from_ymd_opt(digits(y)? as i32, digits(m)?, digits(d)?)
        }
        _ => None,
    }
}

/// Parse a clock reading: `HH`, `HHMM`, `HHMMSS[.f]`, `HH:MM`, `HH:MM:SS[.f]`
/// (fraction of 1-9 digits). No zone suffix is accepted.
pub(super) fn parse_time_text(text: &str) -> Option<NaiveTime> {
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

/// Parse `<date>[T<time>]` where both halves take any spelling above; a bare
/// date is midnight. No zone suffix is accepted.
pub(super) fn parse_local_datetime_text(text: &str) -> Option<NaiveDateTime> {
    let s = text.trim();
    match s.split_once('T') {
        Some((date, time)) => Some(parse_date_text(date)?.and_time(parse_time_text(time)?)),
        None => parse_date_text(s)?.and_hms_opt(0, 0, 0),
    }
}

/// Render a clock the way openCypher prints a local time: seconds are
/// omitted when zero (and no fraction), the fraction is printed in groups of
/// three digits (milli-, micro-, nanoseconds).
pub(crate) fn format_local_time(t: NaiveTime) -> String {
    let nanos = t.nanosecond();
    if nanos == 0 && t.second() == 0 {
        return format!("{:02}:{:02}", t.hour(), t.minute());
    }
    let base = format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second());
    if nanos == 0 {
        base
    } else if nanos.is_multiple_of(1_000_000) {
        format!("{base}.{:03}", nanos / 1_000_000)
    } else if nanos.is_multiple_of(1_000) {
        format!("{base}.{:06}", nanos / 1_000)
    } else {
        format!("{base}.{nanos:09}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn date_spellings() {
        for (text, expected) in [
            ("2015-07-21", ymd(2015, 7, 21)),
            ("20150721", ymd(2015, 7, 21)),
            ("2015-07", ymd(2015, 7, 1)),
            ("201507", ymd(2015, 7, 1)),
            ("2015-W30-2", ymd(2015, 7, 21)),
            ("2015W302", ymd(2015, 7, 21)),
            ("2015-W30", ymd(2015, 7, 20)),
            ("2015W30", ymd(2015, 7, 20)),
            ("2015-202", ymd(2015, 7, 21)),
            ("2015202", ymd(2015, 7, 21)),
            ("2015", ymd(2015, 1, 1)),
        ] {
            assert_eq!(parse_date_text(text), Some(expected), "{text}");
        }
        for bad in [
            "2015-W30-",
            "2015-W3",
            "2015W30-2",
            "2015-W54",
            "2015-367",
            "x",
            "",
        ] {
            assert_eq!(parse_date_text(bad), None, "{bad}");
        }
    }

    #[test]
    fn time_spellings_and_rendering() {
        for (text, rendered) in [
            ("21:40:32.142", "21:40:32.142"),
            ("214032.142", "21:40:32.142"),
            ("21:40:32", "21:40:32"),
            ("214032", "21:40:32"),
            ("21:40", "21:40"),
            ("2140", "21:40"),
            ("21", "21:00"),
            ("12:31:14.000000003", "12:31:14.000000003"),
            ("12:31:14.645876", "12:31:14.645876"),
        ] {
            assert_eq!(
                format_local_time(parse_time_text(text).unwrap()),
                rendered,
                "{text}"
            );
        }
        for bad in ["21:40.5", "2140.5", "24:00", "21:4", "21:40:32.", "x"] {
            assert_eq!(parse_time_text(bad), None, "{bad}");
        }
    }

    #[test]
    fn week_dates_across_year_boundaries() {
        assert_eq!(date_from_iso_week(1817, 1, 1), Ok(ymd(1816, 12, 30)));
        assert_eq!(date_from_iso_week(1818, 53, 1), Ok(ymd(1818, 12, 28)));
        assert!(date_from_iso_week(1816, 53, 1).is_err());
        assert_eq!(day_of_quarter(ymd(1984, 11, 11)), 42);
    }
}
