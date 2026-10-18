//! Host tests of zone resolution over small sources, each exercising one of
//! `zic`'s behaviours; every real zone is held to `zic`'s own output by the
//! oracle test.

use tairix_tz::rule::{DayRule, Moment, TimeBase};

use super::{resolve, Future, Phase, Resolved, Type, ZoneError};
use crate::source::parse;

const HOUR: i64 = 3600;

fn resolution(text: &str) -> Result<Resolved, ZoneError> {
    let source = parse(text).expect("the source parses");
    let book = crate::rule_book(&source.rules);
    let plans = crate::plans(&source.zones[0], &book).expect("every era plans");
    resolve(&plans)
}

fn resolved(text: &str) -> Resolved {
    resolution(text).expect("the zone resolves")
}

fn kind(offset: i64, abbreviation: &str, daylight: bool) -> Type {
    Type {
        offset,
        abbreviation: String::from(abbreviation),
        daylight,
    }
}

/// The UT second of a civil date and time.
fn utc(year: i64, month: u32, day: u32, hour: i64) -> i64 {
    tairix_abi::time::days_from_civil(year, month, day).expect("a representable date") * 86_400
        + hour * HOUR
}

fn last_sunday(month: u8, at: i32) -> Moment {
    Moment {
        month,
        day: DayRule::OnOrBefore {
            weekday: 0,
            day: if month == 2 { 29 } else { 31 },
        },
        at,
        base: TimeBase::Universal,
    }
}

#[test]
fn a_fixed_zone_holds_one_type_for_ever() {
    let zone = resolved("Z T/Fixed 5:30 - IST\n");
    assert_eq!(zone.initial, kind(19_800, "IST", false));
    assert!(zone.transitions.is_empty());
    assert_eq!(zone.future, Future::Standard(kind(19_800, "IST", false)));
    let always = resolved("Z T/Always -4 1 -03\n");
    assert_eq!(
        always.future,
        Future::Daylight(kind(-3 * HOUR, "-03", true)),
        "daylight saving all year"
    );
}

#[test]
fn a_percent_z_format_spells_the_offset_in_force() {
    for (offset, spelled) in [
        ("5:45", "+0545"),
        ("-3:30", "-0330"),
        ("0:1:15", "+000115"),
        ("-10", "-10"),
    ] {
        let zone = resolved(&format!("Z T/Z {offset} - %z\n"));
        assert_eq!(zone.initial.abbreviation, spelled, "{offset}");
    }
    assert_eq!(resolution("Z T/Z 100 - %z\n"), Err(ZoneError::LongOffset));
}

const EUROPEAN: &str = "\
R E 1979 1995 - S lastSu 1u 0 -
R E 1981 ma - Mar lastSu 1u 1 S
R E 1996 ma - O lastSu 1u 0 -
Z T/Europe -0:1:15 - LMT 1847 D
0 E GMT/BST
";

#[test]
fn local_mean_time_gives_way_to_rules_that_alternate_for_ever() {
    let zone = resolved(EUROPEAN);
    assert_eq!(
        zone.initial,
        kind(-75, "LMT", false),
        "before standard time"
    );
    assert_eq!(
        zone.transitions[0],
        (utc(1847, 12, 1, 0) + 75, kind(0, "GMT", false))
    );
    // Every change up to the first the alternating rules govern is explicit,
    // and that one is kept so a reader knows where the rules take over.
    assert_eq!(
        zone.transitions.last(),
        Some(&(utc(1996, 3, 31, 1), kind(HOUR, "BST", true)))
    );
    assert!(zone
        .transitions
        .contains(&(utc(1995, 9, 24, 1), kind(0, "GMT", false))));
    assert_eq!(
        zone.future,
        Future::Alternating {
            stdoff: 0,
            daylight: Phase {
                save: HOUR,
                abbreviation: String::from("BST"),
                moment: last_sunday(3, 3600),
            },
            standard: Phase {
                save: 0,
                abbreviation: String::from("GMT"),
                moment: last_sunday(10, 3600),
            },
        }
    );
}

#[test]
fn negative_saving_makes_winter_the_daylight_phase() {
    let zone = resolved(
        "R N 1981 ma - Mar lastSu 1u 0 -\nR N 1996 ma - O lastSu 1u -1 -\nZ T/Neg 1 N IST/GMT\n",
    );
    assert_eq!(zone.initial, kind(HOUR, "IST", false));
    let Future::Alternating {
        daylight, standard, ..
    } = zone.future
    else {
        panic!("alternating rules");
    };
    assert_eq!(
        (daylight.save, daylight.abbreviation.as_str()),
        (-HOUR, "GMT")
    );
    assert_eq!((standard.save, standard.abbreviation.as_str()), (0, "IST"));
}

#[test]
fn an_era_begun_in_summer_starts_in_the_saving_its_rules_had_reached() {
    let zone = resolved(
        "R P 1990 ma - Mar lastSu 1u 1 -\nR P 1990 ma - O lastSu 1u 0 -\nZ T/Mid 0 - XMT 2000 Jul\n0 P GMT/BST\n",
    );
    assert_eq!(zone.initial, kind(0, "XMT", false));
    assert_eq!(
        zone.transitions[0],
        (utc(2000, 7, 1, 0), kind(HOUR, "BST", true))
    );
}

#[test]
fn two_rules_at_one_instant_are_refused() {
    assert_eq!(
        resolution("R D 2000 o - Mar 1 0 1 S\nR D 2000 o - Mar 1 0 0 -\nZ T/Dup 0 D X%sX\n"),
        Err(ZoneError::SameInstant)
    );
}

#[test]
fn with_no_rule_for_the_future_four_centuries_are_written_out() {
    let zone =
        resolved("R U 2000 ma - Mar 1 0 0 A\nR U 2000 ma - S 1 0 0 B\nZ T/Unknown 0 U %sST\n");
    assert_eq!(
        zone.future,
        Future::Unknown,
        "two rules of standard time for ever"
    );
    assert_eq!(zone.transitions.len(), 2 * (2402 - 2000 + 1));
    assert_eq!(
        zone.transitions.last(),
        Some(&(utc(2402, 9, 1, 0), kind(0, "BST", false)))
    );
    // Rules that stop early are claimed to the end of the four centuries.
    let ended =
        resolved("R Q 2000 2005 - Mar 1 0 0 A\nR Q 2000 2005 - Mar 1 12 0 B\nZ T/Ended 0 Q %sST\n");
    assert_eq!(ended.future, Future::Unknown, "two equal last rules");
    assert_eq!(
        ended.transitions.last(),
        Some(&(utc(2408, 1, 1, 0), kind(0, "BST", false)))
    );
}

#[test]
fn a_last_type_its_continuation_contradicts_is_refused() {
    // A standard saving: the TZ string's standard time drops it.
    assert_eq!(resolution("Z T/Odd 0 1s XST\n"), Err(ZoneError::Disagrees));
}

/// A weekday on or after the 29th, or on or before the 6th, is a day a TZ
/// string cannot state, so a zone continuing on one is refused rather than
/// read otherwise than `zic`'s output reads it.
#[test]
fn a_continuing_rule_on_a_day_no_tz_string_states_is_refused() {
    for day in ["Sun>=29", "Sun>=31", "Sun<=6", "Sun<=1"] {
        assert_eq!(
            resolution(&format!(
                "R E 2000 ma - Mar {day} 1u 1 S\nR E 2000 ma - O lastSun 1u 0 -\nZ T/Edge 0 E X%sT\n"
            )),
            Err(ZoneError::Inexact),
            "{day}"
        );
    }
    for day in ["Sun>=28", "Sun<=7", "lastSun", "Sun>=1"] {
        assert!(
            resolution(&format!(
                "R E 2000 ma - Mar {day} 1u 1 S\nR E 2000 ma - O lastSun 1u 0 -\nZ T/Edge 0 E X%sT\n"
            ))
            .is_ok(),
            "{day}"
        );
    }
}

/// An era whose start no rule has yet named the letters for, and an era that
/// lasts no time at all, are refused rather than given an invented
/// abbreviation or two types at one instant.
#[test]
fn an_untold_abbreviation_and_a_doubled_transition_are_refused() {
    assert_eq!(
        resolution("R N 2000 o - Mar 1 0 1 S\nZ T/Untold 0 - XST 1999\n0 N X%sT\n"),
        Err(ZoneError::NoAbbreviation)
    );
    // The second era ends at 00:00 UT, the instant it begins.
    assert_eq!(
        resolution("Z T/Twice 1 - A 2000 Mar 1 1:00\n1:30 - B 2000 Mar 1 1:30\n2 - C\n"),
        Err(ZoneError::SameTransition)
    );
}
