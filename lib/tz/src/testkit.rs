//! A small store of real zones and regions, built through the one writer:
//! this crate's fixture, and other crates' through `test-support`.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::time::days_from_civil;

use crate::layout::seal;
use crate::rule::{leap_month_length, DayRule, Moment, TimeBase};
use crate::write::{
    build, PhaseInput, PlaceInput, RegionInput, StoreInput, TrailingInput, TransitionInput,
    ZoneInput,
};

/// Seconds in an hour.
pub const HOUR: i32 = 3600;

/// The UT second of a civil date and time.
#[must_use]
pub fn utc(year: i64, month: u32, day: u32, hour: i64, minute: i64) -> i64 {
    days_from_civil(year, month, day).expect("a representable date") * 86_400
        + hour * 3600
        + minute * 60
}

/// A change to `offset` at the UT second `at`.
#[must_use]
pub fn transition(at: i64, offset: i32, abbreviation: &str, daylight: bool) -> TransitionInput {
    TransitionInput {
        at,
        offset,
        abbreviation: String::from(abbreviation),
        daylight,
    }
}

fn last_sunday(month: u8, at: i32, base: TimeBase) -> Moment {
    Moment {
        month,
        day: DayRule::OnOrBefore {
            weekday: 0,
            day: leap_month_length(month),
        },
        at,
        base,
    }
}

fn phase(save: i32, abbreviation: &str, moment: Moment) -> PhaseInput {
    PhaseInput {
        save,
        abbreviation: String::from(abbreviation),
        moment,
    }
}

fn region(code: &str, name: &str, date: &str, time: &str) -> RegionInput {
    RegionInput {
        code: String::from(code),
        name: String::from(name),
        date: String::from(date),
        time: String::from(time),
    }
}

fn place(zone: &str, region: &str, comment: &str) -> PlaceInput {
    PlaceInput {
        zone: String::from(zone),
        region: String::from(region),
        comment: String::from(comment),
    }
}

/// London (the EU rules, both 2026 changes explicit), New York (the US rules
/// from the start of time), Kolkata (fixed), and Dublin (negative saving:
/// winter is the saved phase).
#[must_use]
pub fn store_input() -> StoreInput {
    let london = ZoneInput {
        name: String::from("Europe/London"),
        transitions: vec![
            transition(i64::MIN, -75, "LMT", false),
            transition(utc(1847, 12, 1, 0, 0) + 75, 0, "GMT", false),
            transition(utc(2026, 3, 29, 1, 0), HOUR, "BST", true),
            transition(utc(2026, 10, 25, 1, 0), 0, "GMT", false),
        ],
        trailing: Some(TrailingInput {
            stdoff: 0,
            daylight: phase(HOUR, "BST", last_sunday(3, HOUR, TimeBase::Universal)),
            standard: phase(0, "GMT", last_sunday(10, HOUR, TimeBase::Universal)),
        }),
    };
    let new_york = ZoneInput {
        name: String::from("America/New_York"),
        transitions: vec![transition(i64::MIN, -5 * HOUR, "EST", false)],
        trailing: Some(TrailingInput {
            stdoff: -5 * HOUR,
            daylight: phase(
                HOUR,
                "EDT",
                Moment {
                    month: 3,
                    day: DayRule::OnOrAfter { weekday: 0, day: 8 },
                    at: 2 * HOUR,
                    base: TimeBase::Wall,
                },
            ),
            standard: phase(
                0,
                "EST",
                Moment {
                    month: 11,
                    day: DayRule::OnOrAfter { weekday: 0, day: 1 },
                    at: 2 * HOUR,
                    base: TimeBase::Wall,
                },
            ),
        }),
    };
    let kolkata = ZoneInput {
        name: String::from("Asia/Kolkata"),
        transitions: vec![transition(i64::MIN, 19_800, "IST", false)],
        trailing: None,
    };
    let dublin = ZoneInput {
        name: String::from("Europe/Dublin"),
        transitions: vec![transition(i64::MIN, 0, "GMT", true)],
        trailing: Some(TrailingInput {
            stdoff: HOUR,
            daylight: phase(-HOUR, "GMT", last_sunday(10, 2 * HOUR, TimeBase::Standard)),
            standard: phase(0, "IST", last_sunday(3, HOUR, TimeBase::Universal)),
        }),
    };
    StoreInput {
        release: String::from("2026e"),
        zones: vec![london, new_york, kolkata, dublin],
        aliases: vec![(
            String::from("Europe/Belfast"),
            String::from("Europe/London"),
        )],
        regions: vec![
            region("US", "United States", "M/d/y", "h:mm a"),
            region("GB", "United Kingdom", "dd/MM/y", "HH:mm"),
            region("IN", "India", "d/M/y", "h:mm a"),
            region("IE", "Ireland", "dd/MM/y", "HH:mm"),
        ],
        places: vec![
            place("Europe/London", "GB", ""),
            place("America/New_York", "US", "Eastern (most areas)"),
            place("Asia/Kolkata", "IN", ""),
            place("Europe/Dublin", "IE", ""),
            place("Europe/Belfast", "GB", "Northern Ireland"),
        ],
        default_zone: String::from("Europe/London"),
        default_region: String::from("GB"),
    }
}

/// The fixture store's bytes.
#[must_use]
pub fn store() -> Vec<u8> {
    build(&store_input()).expect("the fixture builds")
}

/// The bytes of the fixture store with `zone` added.
#[must_use]
pub fn store_with(zone: ZoneInput) -> Vec<u8> {
    let mut input = store_input();
    input.zones.push(zone);
    build(&input).expect("the fixture builds")
}

/// Recompute `bytes`' checksum after a test has altered them, so the change
/// reaches the validation it is meant to meet; bytes too short to hold a
/// header are left as they are.
pub fn reseal(bytes: &mut [u8]) {
    seal(bytes);
}
