//! Named IANA time zones for [`Value::ZonedDateTime`](super::Value::ZonedDateTime).
//!
//! [`ZoneId`] is kglite's own handle: a 2-byte index into the bundled tz
//! database, so the public API never names the database crate and the same
//! type exists with the `iana-zones` feature off. Without the feature a
//! [`ZoneId`] cannot be built ([`ZoneId::from_name`] answers `None`), and a
//! stored named-zone value decodes as its fixed offset.
//!
//! The persisted form of a zone is its **name** (stable across database
//! versions), never the index. The index is only meaningful inside one build.
use chrono::NaiveDateTime;

#[cfg(feature = "iana-zones")]
type Repr = std::num::NonZeroU16;
#[cfg(not(feature = "iana-zones"))]
type Repr = std::convert::Infallible;

/// Whether this build carries the IANA time-zone database.
pub const IANA_ZONES: bool = cfg!(feature = "iana-zones");

/// A named time zone (`Europe/Oslo`). Equality is zone identity; two
/// [`Value::ZonedDateTime`](super::Value::ZonedDateTime) values are still equal
/// by instant whatever their zones.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ZoneId(Repr);

/// How a wall-clock reading sits in a zone's rules.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LocalFit {
    /// Exactly one instant has this reading.
    Exact,
    /// A clock change repeated the reading; the earlier offset (or the
    /// preferred one, when it is one of the two) was taken.
    Overlap,
    /// A clock change skipped the reading; it was moved forward by the length
    /// of the gap.
    Gap,
}

/// An instant and the offset a zone has at it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ZoneInstant {
    pub utc: NaiveDateTime,
    pub offset_secs: i32,
    pub fit: LocalFit,
}

impl ZoneId {
    /// The zone called `name` (`Europe/Oslo`, `UTC`, `US/Pacific`); `None` for
    /// an unknown name, and always `None` without the `iana-zones` feature.
    pub fn from_name(name: &str) -> Option<ZoneId> {
        imp::from_name(name)
    }

    /// The IANA name this zone was looked up by.
    pub fn name(self) -> &'static str {
        imp::name(self)
    }

    /// The UTC offset in seconds the zone has at the instant `utc`.
    pub fn offset_at(self, utc: NaiveDateTime) -> i32 {
        imp::offset_at(self, utc)
    }

    /// The instant a wall-clock reading names in this zone.
    ///
    /// A reading in a **gap** (clocks went forward) is shifted forward by the
    /// length of the gap, so `02:30` on the night Oslo skips 02:00-03:00 is
    /// `03:30+02:00`. A reading in an **overlap** (clocks went back) takes the
    /// earlier instant, i.e. the offset from before the change, unless
    /// `preferred_offset` is the other one. `None` when the instant is out of
    /// the representable range.
    pub fn resolve_local(
        self,
        wall: NaiveDateTime,
        preferred_offset: Option<i32>,
    ) -> Option<ZoneInstant> {
        imp::resolve_local(self, wall, preferred_offset)
    }

    /// The same instant seen in this zone.
    pub fn at_instant(self, utc: NaiveDateTime) -> ZoneInstant {
        ZoneInstant {
            utc,
            offset_secs: self.offset_at(utc),
            fit: LocalFit::Exact,
        }
    }
}

#[cfg(feature = "iana-zones")]
mod imp {
    use super::{LocalFit, ZoneId, ZoneInstant};
    use chrono::{LocalResult, NaiveDateTime, Offset, TimeDelta, TimeZone};
    use chrono_tz::{Tz, TZ_VARIANTS};
    use std::num::NonZeroU16;
    use std::str::FromStr;

    fn tz(id: ZoneId) -> Tz {
        TZ_VARIANTS[usize::from(id.0.get()) - 1]
    }

    pub(super) fn from_name(name: &str) -> Option<ZoneId> {
        let tz = Tz::from_str(name).ok()?;
        let index = u16::try_from(tz as usize).ok()?.checked_add(1)?;
        NonZeroU16::new(index).map(ZoneId)
    }

    pub(super) fn name(id: ZoneId) -> &'static str {
        tz(id).name()
    }

    pub(super) fn offset_at(id: ZoneId, utc: NaiveDateTime) -> i32 {
        tz(id)
            .offset_from_utc_datetime(&utc)
            .fix()
            .local_minus_utc()
    }

    fn offset_from(wall: NaiveDateTime, id: ZoneId, hours: i64) -> Option<i32> {
        let probe = wall.checked_add_signed(TimeDelta::try_hours(hours)?)?;
        Some(offset_at(id, probe))
    }

    pub(super) fn resolve_local(
        id: ZoneId,
        wall: NaiveDateTime,
        preferred: Option<i32>,
    ) -> Option<ZoneInstant> {
        let at = |offset_secs: i32, fit| -> Option<ZoneInstant> {
            let utc = wall.checked_sub_signed(TimeDelta::try_seconds(i64::from(offset_secs))?)?;
            Some(ZoneInstant {
                utc,
                offset_secs,
                fit,
            })
        };
        match tz(id).offset_from_local_datetime(&wall) {
            LocalResult::Single(o) => at(o.fix().local_minus_utc(), LocalFit::Exact),
            LocalResult::Ambiguous(earlier, later) => {
                let (earlier, later) = (
                    earlier.fix().local_minus_utc(),
                    later.fix().local_minus_utc(),
                );
                let offset = if preferred == Some(later) {
                    later
                } else {
                    earlier
                };
                at(offset, LocalFit::Overlap)
            }
            LocalResult::None => {
                // A skipped reading: read it with the offset from before the
                // change, which lands after the change, and show it with the
                // offset from after (the wall clock moves forward by the gap).
                // Two days either side clears any clock change that is not
                // itself within two days of another.
                let before = offset_from(wall, id, -48)?;
                let instant = at(before, LocalFit::Gap)?;
                Some(ZoneInstant {
                    offset_secs: offset_at(id, instant.utc),
                    ..instant
                })
            }
        }
    }

    #[cfg(test)]
    pub(super) fn variant_count() -> usize {
        TZ_VARIANTS.len()
    }
}

#[cfg(not(feature = "iana-zones"))]
mod imp {
    use super::{ZoneId, ZoneInstant};
    use chrono::NaiveDateTime;

    pub(super) fn from_name(_name: &str) -> Option<ZoneId> {
        None
    }

    // Without the feature no `ZoneId` exists, so these cannot run.
    pub(super) fn name(id: ZoneId) -> &'static str {
        match id.0 {}
    }

    pub(super) fn offset_at(id: ZoneId, _utc: NaiveDateTime) -> i32 {
        match id.0 {}
    }

    pub(super) fn resolve_local(
        id: ZoneId,
        _wall: NaiveDateTime,
        _preferred: Option<i32>,
    ) -> Option<ZoneInstant> {
        match id.0 {}
    }
}

#[cfg(all(test, feature = "iana-zones"))]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn wall(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    fn zone(name: &str) -> ZoneId {
        ZoneId::from_name(name).unwrap()
    }

    #[test]
    fn every_variant_round_trips_through_its_name() {
        for i in 1..=imp::variant_count() {
            let id = ZoneId(std::num::NonZeroU16::new(i as u16).unwrap());
            assert_eq!(ZoneId::from_name(id.name()), Some(id), "{}", id.name());
        }
        assert_eq!(ZoneId::from_name("Mars/Olympus"), None);
        assert_eq!(ZoneId::from_name(""), None);
        assert_eq!(zone("US/Pacific").name(), "US/Pacific");
    }

    // Oslo: UTC+1 until 2026-03-29 01:00Z, UTC+2 until 2026-10-25 01:00Z.
    #[test]
    fn oslo_spring_forward_shifts_a_skipped_reading_by_the_gap() {
        let oslo = zone("Europe/Oslo");
        // 02:30 does not exist on 2026-03-29 (02:00 -> 03:00): it becomes
        // 03:30+02:00, the instant 01:30Z.
        let r = oslo.resolve_local(wall(2026, 3, 29, 2, 30), None).unwrap();
        assert_eq!(r.fit, LocalFit::Gap);
        assert_eq!(r.offset_secs, 7200);
        assert_eq!(r.utc, wall(2026, 3, 29, 1, 30));
        // The reading just before and just after the gap are exact.
        let before = oslo.resolve_local(wall(2026, 3, 29, 1, 59), None).unwrap();
        assert_eq!((before.fit, before.offset_secs), (LocalFit::Exact, 3600));
        let after = oslo.resolve_local(wall(2026, 3, 29, 3, 0), None).unwrap();
        assert_eq!((after.fit, after.offset_secs), (LocalFit::Exact, 7200));
        assert_eq!(after.utc, wall(2026, 3, 29, 1, 0));
    }

    #[test]
    fn oslo_fall_back_takes_the_earlier_offset_unless_preferred() {
        let oslo = zone("Europe/Oslo");
        // 02:30 happens twice on 2026-10-25 (+02:00 at 00:30Z, +01:00 at 01:30Z).
        let first = oslo.resolve_local(wall(2026, 10, 25, 2, 30), None).unwrap();
        assert_eq!(first.fit, LocalFit::Overlap);
        assert_eq!(
            (first.offset_secs, first.utc),
            (7200, wall(2026, 10, 25, 0, 30))
        );
        let second = oslo
            .resolve_local(wall(2026, 10, 25, 2, 30), Some(3600))
            .unwrap();
        assert_eq!(
            (second.offset_secs, second.utc),
            (3600, wall(2026, 10, 25, 1, 30))
        );
        // A preference that is neither offset falls back to the earlier one.
        let other = oslo
            .resolve_local(wall(2026, 10, 25, 2, 30), Some(0))
            .unwrap();
        assert_eq!(other.offset_secs, 7200);
    }

    // Sydney (southern hemisphere): UTC+11 until 2026-04-05 03:00 local
    // (2026-04-04 16:00Z), then UTC+10 until 2026-10-04 02:00 local
    // (2026-10-03 16:00Z), then UTC+11.
    #[test]
    fn sydney_runs_the_changes_in_the_other_season() {
        let sydney = zone("Australia/Sydney");
        // Autumn 2026-04-05: 02:30 happens twice; the earlier is +11:00.
        let r = sydney.resolve_local(wall(2026, 4, 5, 2, 30), None).unwrap();
        assert_eq!((r.fit, r.offset_secs), (LocalFit::Overlap, 39_600));
        assert_eq!(r.utc, wall(2026, 4, 4, 15, 30));
        // Spring 2026-10-04: 02:30 is skipped (02:00 -> 03:00), becoming 03:30+11:00.
        let g = sydney
            .resolve_local(wall(2026, 10, 4, 2, 30), None)
            .unwrap();
        assert_eq!((g.fit, g.offset_secs), (LocalFit::Gap, 39_600));
        assert_eq!(g.utc, wall(2026, 10, 3, 16, 30));
    }

    #[test]
    fn a_whole_skipped_day_moves_forward_a_day() {
        // Samoa jumped from 2011-12-29 23:59:59 (-10:00) to 2011-12-31 00:00 (+14:00).
        let apia = zone("Pacific/Apia");
        let r = apia.resolve_local(wall(2011, 12, 30, 12, 0), None).unwrap();
        assert_eq!((r.fit, r.offset_secs), (LocalFit::Gap, 14 * 3600));
        assert_eq!(r.utc, wall(2011, 12, 30, 22, 0));
    }

    #[test]
    fn offset_at_follows_the_instant() {
        let oslo = zone("Europe/Oslo");
        assert_eq!(oslo.offset_at(wall(2026, 3, 29, 0, 59)), 3600);
        assert_eq!(oslo.offset_at(wall(2026, 3, 29, 1, 0)), 7200);
        assert_eq!(zone("UTC").offset_at(wall(2026, 6, 1, 0, 0)), 0);
    }
}
