//! Host tests of the annual rule arithmetic: the day a rule names in a given
//! year, and the instant that is on each clock.

use super::{DayRule, Moment, TimeBase};
use crate::testkit::{utc, HOUR};

#[test]
fn a_weekday_on_or_after_a_day_is_sought_forward() {
    // The American spring change: the second Sunday of March, 02:00 local.
    let spring = Moment {
        month: 3,
        day: DayRule::OnOrAfter { weekday: 0, day: 8 },
        at: 2 * HOUR,
        base: TimeBase::Wall,
    };
    assert_eq!(spring.local_seconds(2026), Some(utc(2026, 3, 8, 2, 0)));
    assert_eq!(spring.local_seconds(2027), Some(utc(2027, 3, 14, 2, 0)));
}

#[test]
fn a_last_weekday_is_sought_backward_from_the_months_end() {
    let autumn = Moment {
        month: 10,
        day: DayRule::OnOrBefore {
            weekday: 0,
            day: 31,
        },
        at: HOUR,
        base: TimeBase::Universal,
    };
    assert_eq!(autumn.local_seconds(2026), Some(utc(2026, 10, 25, 1, 0)));
    // The last Sunday of February bounds on the 29th, which only a leap
    // year has; otherwise the search starts from the 28th.
    let february = Moment {
        month: 2,
        day: DayRule::OnOrBefore {
            weekday: 0,
            day: 29,
        },
        at: 0,
        base: TimeBase::Wall,
    };
    assert_eq!(february.local_seconds(2026), Some(utc(2026, 2, 22, 0, 0)));
    assert_eq!(february.local_seconds(2032), Some(utc(2032, 2, 29, 0, 0)));
}

#[test]
fn the_29th_of_february_is_refused_in_a_year_without_one() {
    let leap_day = Moment {
        month: 2,
        day: DayRule::Of(29),
        at: 0,
        base: TimeBase::Wall,
    };
    assert_eq!(leap_day.local_seconds(2024), Some(utc(2024, 2, 29, 0, 0)));
    assert_eq!(leap_day.local_seconds(2025), None);
    assert!(!leap_day.is_well_formed(), "not every year has it");
}

#[test]
fn the_instant_is_read_on_the_rules_own_clock() {
    let moment = |base| Moment {
        month: 3,
        day: DayRule::Of(29),
        at: HOUR,
        base,
    };
    let local = utc(2026, 3, 29, 1, 0);
    let (stdoff, save) = (HOUR, HOUR / 2);
    assert_eq!(
        moment(TimeBase::Universal).instant(2026, stdoff, save),
        Some(local)
    );
    assert_eq!(
        moment(TimeBase::Standard).instant(2026, stdoff, save),
        Some(local - 3600)
    );
    assert_eq!(
        moment(TimeBase::Wall).instant(2026, stdoff, save),
        Some(local - 5400)
    );
}

#[test]
fn a_time_of_day_past_midnight_lands_on_a_following_day() {
    let late = Moment {
        month: 4,
        day: DayRule::OnOrAfter { weekday: 6, day: 1 },
        at: 25 * HOUR,
        base: TimeBase::Wall,
    };
    // The first Saturday of April 2026 is the 4th; 25:00 is 01:00 on the 5th.
    assert_eq!(late.local_seconds(2026), Some(utc(2026, 4, 5, 1, 0)));
}

#[test]
fn a_year_far_from_now_fails_rather_than_overflowing() {
    let moment = Moment {
        month: 1,
        day: DayRule::Of(1),
        at: 0,
        base: TimeBase::Wall,
    };
    assert_eq!(moment.local_seconds(i64::MAX / 100), None);
    assert!(
        moment.local_seconds(-4000).is_some(),
        "years before 1 resolve"
    );
}

#[test]
fn a_moment_outside_the_calendar_is_not_well_formed() {
    let base = Moment {
        month: 13,
        day: DayRule::Of(1),
        at: 0,
        base: TimeBase::Wall,
    };
    assert!(!base.is_well_formed());
    let weekday = Moment {
        month: 3,
        day: DayRule::OnOrAfter { weekday: 7, day: 8 },
        ..base
    };
    assert!(!weekday.is_well_formed());
    let april_31 = Moment {
        month: 4,
        day: DayRule::Of(31),
        ..base
    };
    assert!(!april_31.is_well_formed());
    let last_sunday = Moment {
        month: 2,
        day: DayRule::OnOrBefore {
            weekday: 0,
            day: 29,
        },
        ..base
    };
    assert!(last_sunday.is_well_formed());
}

/// A weekday a caller built past Saturday names no day, rather than one a
/// week out or a panic.
#[test]
fn a_weekday_past_saturday_names_no_day() {
    for day in [
        DayRule::OnOrBefore {
            weekday: 14,
            day: 15,
        },
        DayRule::OnOrAfter { weekday: 7, day: 8 },
    ] {
        let moment = Moment {
            month: 3,
            day,
            at: 0,
            base: TimeBase::Universal,
        };
        assert_eq!(moment.local_seconds(2026), None);
    }
}
