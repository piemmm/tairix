//! Host tests of zone evaluation: the offset in force on both sides of every
//! kind of change, local time in both directions, and the ends of time.

use tairix_abi::time::{CivilTime, Time64};

use alloc::string::String;
use alloc::vec;

use crate::testkit::{store, store_input, store_with, transition, utc, HOUR};
use crate::write::{build, TransitionInput};
use crate::{Disambiguation, LocalError, Zone, ZoneInfo};

fn offset(zone: &Zone<'_>, at: i64) -> (i32, &'static str, bool) {
    let offset = zone.offset_at(Time64::from_secs(at));
    let name: &'static str = match offset.abbreviation {
        "GMT" => "GMT",
        "BST" => "BST",
        "EST" => "EST",
        "EDT" => "EDT",
        "IST" => "IST",
        "LMT" => "LMT",
        "UTC" => "UTC",
        other => panic!("unexpected abbreviation {other}"),
    };
    (offset.utc_offset, name, offset.daylight)
}

fn civil(year: i64, month: u32, day: u32, hour: u32, minute: u32) -> CivilTime {
    CivilTime {
        year,
        month,
        day,
        hour,
        minute,
        second: 0,
    }
}

#[test]
fn an_explicit_transition_takes_effect_at_its_instant() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let london = info.zone("Europe/London").expect("London");
    let spring = utc(2026, 3, 29, 1, 0);
    assert_eq!(offset(&london, spring - 1), (0, "GMT", false));
    assert_eq!(offset(&london, spring), (HOUR, "BST", true));
    assert_eq!(
        offset(&london, utc(1700, 1, 1, 0, 0)),
        (-75, "LMT", false),
        "before 1970"
    );
    assert_eq!(offset(&london, utc(1950, 6, 1, 0, 0)), (0, "GMT", false));
}

#[test]
fn the_trailing_rule_carries_a_zone_past_its_last_transition() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let london = info.zone("Europe/London").expect("London");
    // The last explicit transition is October 2026; 2031 and 2100 are the
    // rule's alone. The last Sunday of March 2031 is the 30th.
    let spring = utc(2031, 3, 30, 1, 0);
    assert_eq!(offset(&london, spring - 1), (0, "GMT", false));
    assert_eq!(offset(&london, spring), (HOUR, "BST", true));
    let autumn = utc(2031, 10, 26, 1, 0);
    assert_eq!(offset(&london, autumn - 1), (HOUR, "BST", true));
    assert_eq!(offset(&london, autumn), (0, "GMT", false));
    assert_eq!(
        offset(&london, utc(2100, 7, 1, 12, 0)),
        (HOUR, "BST", true),
        "after 2038"
    );
    assert_eq!(offset(&london, utc(2100, 1, 1, 0, 0)), (0, "GMT", false));
    assert_eq!(
        offset(&london, i64::MAX),
        (0, "GMT", false),
        "the end of time"
    );
}

#[test]
fn a_wall_clock_rule_reads_its_time_with_the_saving_before_it() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let new_york = info.zone("America/New_York").expect("New York");
    // 2030: the second Sunday of March is the 10th, the first of November the
    // 3rd; both changes are at 02:00 on the clock in force.
    let spring = utc(2030, 3, 10, 7, 0);
    assert_eq!(offset(&new_york, spring - 1), (-5 * HOUR, "EST", false));
    assert_eq!(offset(&new_york, spring), (-4 * HOUR, "EDT", true));
    let autumn = utc(2030, 11, 3, 6, 0);
    assert_eq!(offset(&new_york, autumn - 1), (-4 * HOUR, "EDT", true));
    assert_eq!(offset(&new_york, autumn), (-5 * HOUR, "EST", false));
}

#[test]
fn negative_saving_keeps_winter_as_the_saved_phase() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let dublin = info.zone("Europe/Dublin").expect("Dublin");
    assert_eq!(offset(&dublin, utc(2030, 1, 15, 12, 0)), (0, "GMT", true));
    assert_eq!(
        offset(&dublin, utc(2030, 7, 15, 12, 0)),
        (HOUR, "IST", false)
    );
}

#[test]
fn a_local_reading_applies_the_offset_to_the_calendar() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let new_york = info.zone("America/New_York").expect("New York");
    let reading = new_york.local(Time64::from_secs(utc(2030, 1, 1, 3, 30)));
    assert_eq!(
        reading.civil,
        CivilTime {
            second: 0,
            ..civil(2029, 12, 31, 22, 30)
        }
    );
    assert_eq!(reading.offset.abbreviation, "EST");
}

#[test]
fn a_local_time_names_its_one_instant() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let london = info.zone("Europe/London").expect("London");
    let summer = london.instant_from_local(&civil(2030, 7, 1, 12, 0), Disambiguation::Reject);
    assert_eq!(summer, Ok(Time64::from_secs(utc(2030, 7, 1, 11, 0))));
    let kolkata = info.zone("Asia/Kolkata").expect("Kolkata");
    assert_eq!(
        kolkata.instant_from_local(&civil(1960, 2, 29, 5, 30), Disambiguation::Reject),
        Ok(Time64::from_secs(utc(1960, 2, 29, 0, 0)))
    );
}

#[test]
fn a_time_the_clocks_skipped_is_a_gap() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let london = info.zone("Europe/London").expect("London");
    // 31 March 2030: the clocks went from 01:00 to 02:00.
    for choice in [
        Disambiguation::Reject,
        Disambiguation::Earlier,
        Disambiguation::Later,
    ] {
        assert_eq!(
            london.instant_from_local(&civil(2030, 3, 31, 1, 30), choice),
            Err(LocalError::Gap)
        );
    }
}

#[test]
fn a_time_the_clocks_passed_twice_is_an_overlap_unless_one_is_chosen() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let london = info.zone("Europe/London").expect("London");
    // 27 October 2030: the clocks went from 02:00 back to 01:00.
    let twice = civil(2030, 10, 27, 1, 30);
    assert_eq!(
        london.instant_from_local(&twice, Disambiguation::Reject),
        Err(LocalError::Overlap)
    );
    assert_eq!(
        london.instant_from_local(&twice, Disambiguation::Earlier),
        Ok(Time64::from_secs(utc(2030, 10, 27, 0, 30))),
        "in summer time"
    );
    assert_eq!(
        london.instant_from_local(&twice, Disambiguation::Later),
        Ok(Time64::from_secs(utc(2030, 10, 27, 1, 30))),
        "in winter time"
    );
}

#[test]
fn fields_no_calendar_holds_are_invalid() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let london = info.zone("Europe/London").expect("London");
    for fields in [
        civil(2030, 2, 29, 12, 0),
        civil(2030, 13, 1, 0, 0),
        civil(2030, 1, 1, 24, 0),
    ] {
        assert_eq!(
            london.instant_from_local(&fields, Disambiguation::Reject),
            Err(LocalError::Invalid)
        );
    }
}

#[test]
fn every_instant_reads_back_through_its_local_time() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    for zone in info.zones() {
        let mut at = utc(1990, 1, 1, 0, 0);
        while at < utc(2045, 1, 1, 0, 0) {
            let reading = zone.local(Time64::from_secs(at));
            let back = zone
                .instant_from_local(&reading.civil, Disambiguation::Earlier)
                .expect("a reading the zone made names an instant");
            let again = zone.local(back);
            assert_eq!(again.civil, reading.civil, "{} at {at}", zone.name());
            at += 3 * 86_400 + 3_607;
        }
    }
}

#[test]
fn utc_is_a_zone_of_its_own() {
    assert_eq!(Zone::UTC.name(), "UTC");
    assert_eq!(offset(&Zone::UTC, utc(2030, 7, 1, 0, 0)), (0, "UTC", false));
    assert_eq!(
        Zone::UTC.instant_from_local(&civil(2030, 7, 1, 0, 0), Disambiguation::Reject),
        Ok(Time64::from_secs(utc(2030, 7, 1, 0, 0)))
    );
}

#[test]
fn a_local_time_just_after_the_rules_take_over_names_its_instant() {
    // A store whose rules disagree with its last transition: the offset the
    // rules give from a second after it is gathered all the same.
    let mut input = store_input();
    let london = input
        .zones
        .iter_mut()
        .find(|zone| zone.name == "Europe/London")
        .expect("London");
    let entry = |at, offset, abbreviation: &str| TransitionInput {
        at,
        offset,
        abbreviation: String::from(abbreviation),
        daylight: false,
    };
    london.transitions = vec![
        entry(i64::MIN, 0, "GMT"),
        entry(utc(2026, 3, 1, 0, 0), 5 * HOUR, "LMT"),
        entry(utc(2026, 7, 1, 0, 0), 0, "GMT"),
    ];
    let bytes = build(&input).expect("the store builds");
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let london = info.zone("Europe/London").expect("London");
    assert_eq!(offset(&london, utc(2026, 7, 1, 0, 0)), (0, "GMT", false));
    assert_eq!(
        offset(&london, utc(2026, 7, 1, 0, 0) + 1),
        (HOUR, "BST", true)
    );
    assert_eq!(
        london.instant_from_local(&civil(2026, 7, 1, 12, 0), Disambiguation::Reject),
        Ok(Time64::from_secs(utc(2026, 7, 1, 11, 0)))
    );
}

/// Rules whose times carry both of a year's changes into the next January:
/// the latest change before an instant there is two years back.
#[test]
fn a_rule_whose_changes_land_past_new_year_is_read_from_two_years_back() {
    use crate::rule::{DayRule, Moment, TimeBase};
    use crate::write::{PhaseInput, TrailingInput, ZoneInput};
    let moment = |days: i32| Moment {
        month: 12,
        day: DayRule::Of(31),
        at: days * 24 * HOUR,
        base: TimeBase::Universal,
    };
    let bytes = store_with(ZoneInput {
        name: String::from("Etc/Late"),
        transitions: vec![transition(i64::MIN, 0, "STD", false)],
        trailing: Some(TrailingInput {
            stdoff: 0,
            daylight: PhaseInput {
                save: HOUR,
                abbreviation: String::from("DST"),
                moment: moment(7),
            },
            standard: PhaseInput {
                save: 0,
                abbreviation: String::from("STD"),
                moment: moment(6),
            },
        }),
    });
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let zone = info.zone("Etc/Late").expect("the zone");
    let early_january = zone.offset_at(Time64::from_secs(utc(2030, 1, 3, 0, 0)));
    assert_eq!(
        (early_january.utc_offset, early_january.daylight),
        (HOUR, true),
        "daylight saving began on 7 January 2029, from 2028's rule"
    );
    let after = zone.offset_at(Time64::from_secs(utc(2030, 1, 6, 12, 0)));
    assert_eq!((after.utc_offset, after.daylight), (0, false));
}

/// More offsets in force around a local time than any real zone keeps: the
/// one stretch that reads as it is still found.
#[test]
fn a_local_time_among_many_nearby_offsets_is_found() {
    use crate::write::ZoneInput;
    let start = utc(2030, 6, 1, 0, 0);
    let mut transitions = vec![transition(i64::MIN, -HOUR, "AA", false)];
    for step in 0..10 {
        transitions.push(transition(
            start + i64::from(step) * i64::from(HOUR),
            step * 600,
            &alloc::format!("A{step}"),
            false,
        ));
    }
    let bytes = store_with(ZoneInput {
        name: String::from("Etc/Busy"),
        transitions,
        trailing: None,
    });
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let zone = info.zone("Etc/Busy").expect("the zone");
    // Read only through the ninth of the ten offsets.
    let instant = start + 8 * i64::from(HOUR) + 30 * 60;
    let wall = CivilTime::at_offset(instant, 8 * 600);
    assert_eq!(
        zone.instant_from_local(&wall, Disambiguation::Reject),
        Ok(Time64::from_secs(instant))
    );
}

/// At the ends of the range a local time is exact, and one no instant reaches
/// is a gap rather than a saturated instant that reads otherwise.
#[test]
fn local_time_is_exact_at_the_ends_of_time() {
    use crate::write::ZoneInput;
    let bytes = store_with(ZoneInput {
        name: String::from("Etc/West"),
        transitions: vec![transition(i64::MIN, -5 * HOUR, "W", false)],
        trailing: None,
    });
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let kolkata = info.zone("Asia/Kolkata").expect("Kolkata");
    let last = Time64::from_secs(i64::MAX);
    assert_eq!(
        kolkata.local(last).civil,
        CivilTime::at_offset(i64::MAX, 5 * HOUR + 30 * 60)
    );
    let west = info.zone("Etc/West").expect("the zone");
    let beyond = CivilTime::from_unix_secs(i64::MAX - 10);
    assert_eq!(
        west.instant_from_local(&beyond, Disambiguation::Reject),
        Err(LocalError::Gap),
        "the instant would lie past the end of time"
    );
}

/// A whole-minute zone turns its minutes on the UT minute, local mean time on
/// its own seconds, and a change of offset before the minute comes first.
#[test]
fn a_zone_turns_its_minutes_on_its_own_seconds() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let london = info.default_zone();
    let at = Time64::from_secs;
    let afternoon = utc(2026, 10, 18, 13, 5);
    assert_eq!(london.next_minute(at(afternoon + 20)), at(afternoon + 60));
    assert_eq!(
        london.next_minute(at(afternoon + 60)),
        at(afternoon + 120),
        "never the instant itself"
    );
    // Local mean time, 75 seconds behind UT: 00:00:30 UT reads 23:59:15.
    let mean = utc(1800, 1, 1, 0, 0) + 30;
    assert_eq!(london.next_minute(at(mean)), at(mean + 45));
    let greenwich = utc(1847, 12, 1, 0, 0) + 75;
    assert_eq!(london.next_minute(at(greenwich - 45)), at(greenwich));
    assert_eq!(
        london.next_minute(at(greenwich + 5)),
        at(utc(1847, 12, 1, 0, 2))
    );

    let change = utc(1900, 1, 1, 0, 0);
    let bytes = store_with(crate::write::ZoneInput {
        name: String::from("Etc/Ahead"),
        transitions: vec![
            transition(i64::MIN, 30, "LMT", false),
            transition(change, 0, "UTC", false),
        ],
        trailing: None,
    });
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let ahead = info.zone("Etc/Ahead").expect("the zone");
    assert_eq!(
        ahead.next_minute(at(change - 20)),
        at(change),
        "the change, ahead of the local minute at 00:01:00"
    );
}
