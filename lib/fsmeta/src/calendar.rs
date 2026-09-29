//! The shared spellings of a civil date.
//!
//! The calendar arithmetic itself lives with [`tairix_abi::time::Time64`] in
//! that crate ([`CivilTime`], `days_from_civil`, `civil_from_days`,
//! `weekday_from_days`), because every consumer of an instant already depends
//! on that crate. This module holds only the rendered spellings more than one
//! consumer shares, which need `alloc` and so cannot live there.

use alloc::string::String;
use core::fmt::Write as _;

use tairix_abi::time::{days_from_civil, days_in_month, weekday_from_days, CivilTime};

/// The English three-letter month abbreviations, January first: the C
/// locale's, which GNU `ls` prints.
pub const MONTH_ABBREVIATIONS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The English three-letter weekday abbreviations in ISO 8601 order, Monday
/// first.
pub const WEEKDAY_ABBREVIATIONS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// The abbreviation of month `month`, 1 being January, or `""` for a number no
/// month has: a civil time read from a corrupt source renders short rather than
/// panicking or naming a month it does not have.
#[must_use]
pub fn month_abbreviation(month: u32) -> &'static str {
    ordinal_name(&MONTH_ABBREVIATIONS, month)
}

/// The entry of `names` numbered `ordinal` from 1, or `""` past either end.
fn ordinal_name(names: &[&'static str], ordinal: u32) -> &'static str {
    usize::try_from(ordinal)
        .ok()
        .and_then(|ordinal| ordinal.checked_sub(1))
        .and_then(|index| names.get(index).copied())
        .unwrap_or_default()
}

/// Render `civil` as `YYYY-MM-DD HH:MM` (UTC, minute granularity): the shared
/// long-ISO clock/stamp spelling every minute-granular consumer uses, so the
/// format lives in one place. The year is zero-padded to at least four digits.
#[must_use]
pub fn iso_minute(civil: &CivilTime) -> String {
    let mut out = String::new();
    // Writing into a `String` never fails; the `Result` is discarded
    // deliberately rather than unwrapped.
    let _ = write!(
        out,
        "{:04}-{:02}-{:02} {:02}:{:02}",
        civil.year, civil.month, civil.day, civil.hour, civil.minute
    );
    out
}

/// Render `civil`'s date as a person reads it aloud: `Mon 28 Sep 2026`.
///
/// A month outside the calendar, or a day its month lacks, names nothing
/// rather than panicking, so a civil time read from a corrupt source renders
/// short instead of fabricating a month or a weekday.
#[must_use]
pub fn long_date(civil: &CivilTime) -> String {
    let real = (1..=days_in_month(civil.year, civil.month)).contains(&civil.day);
    let weekday = real
        .then(|| weekday_from_days(days_from_civil(civil.year, civil.month, civil.day)))
        .map_or("", |weekday| ordinal_name(&WEEKDAY_ABBREVIATIONS, weekday));
    let mut out = String::new();
    // Writing into a `String` never fails; the `Result` is discarded
    // deliberately rather than unwrapped.
    let _ = write!(
        out,
        "{} {} {} {}",
        weekday,
        civil.day,
        month_abbreviation(civil.month),
        civil.year
    );
    out
}

#[cfg(test)]
mod tests {
    use super::{iso_minute, long_date, month_abbreviation};
    use tairix_abi::time::CivilTime;

    #[test]
    fn a_month_number_outside_the_calendar_names_nothing() {
        assert_eq!(month_abbreviation(1), "Jan");
        assert_eq!(month_abbreviation(12), "Dec");
        for month in [0, 13, u32::MAX] {
            assert_eq!(month_abbreviation(month), "");
        }
    }

    #[test]
    fn renders_a_known_instant_to_minute_granularity() {
        // 2024-02-29T13:46:07Z — a leap-day, past-2038 anchor.
        let civil = CivilTime::from_unix_secs(1_709_214_367);
        assert_eq!(iso_minute(&civil), "2024-02-29 13:46");
    }

    #[test]
    fn pads_every_component_to_a_fixed_width() {
        // 0001-02-03T04:05:06Z exercises the zero padding on every field.
        let civil = CivilTime {
            year: 1,
            month: 2,
            day: 3,
            hour: 4,
            minute: 5,
            second: 6,
        };
        assert_eq!(iso_minute(&civil), "0001-02-03 04:05");
    }

    #[test]
    fn the_long_date_names_the_weekday_and_the_month() {
        let at = |secs: i64| long_date(&CivilTime::from_unix_secs(secs));
        assert_eq!(at(1_709_214_367), "Thu 29 Feb 2024");
        assert_eq!(at(0), "Thu 1 Jan 1970");
        assert_eq!(at(-1), "Wed 31 Dec 1969");
        // Past the 32-bit rollover, and the far side of 2100's skipped leap day.
        assert_eq!(at(2_147_483_648), "Tue 19 Jan 2038");
        assert_eq!(at(4_107_542_400), "Mon 1 Mar 2100");
    }

    /// Neither a month nor a weekday is named for a date the calendar lacks.
    #[test]
    fn a_date_outside_the_calendar_invents_no_name() {
        let date = |month: u32, day: u32| CivilTime {
            year: 2026,
            month,
            day,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert_eq!(long_date(&date(13, 9)), " 9  2026");
        assert_eq!(long_date(&date(0, 9)), " 9  2026");
        assert_eq!(
            long_date(&date(2, 29)),
            " 29 Feb 2026",
            "2026 is no leap year"
        );
        assert_eq!(long_date(&date(1, 0)), " 0 Jan 2026");
        assert_eq!(long_date(&date(12, 31)), "Thu 31 Dec 2026");
    }
}
