//! The dialog's engine: the readout and its provenance, the zone's next
//! change, a hand-set time composed or refused, and every outcome's words.

use tairix_abi::time::{Time64, WallClockReading, WallTimeState};
use tairix_abi::zone::ZoneName;
use tairix_abi::Errno;
use tairix_sysconfig::SystemConfig;
use tairix_tz::testkit::utc;
use tairix_tz::{CivilClock, Disambiguation, ZoneInfo, ZoneOffset};

use crate::calendar::Date;
use crate::test_support::zones;
use crate::time_entry::TimeOfDay;
use crate::{
    compose, implausible_question, readout, repeated_question, skipped_reason, zone_line, Clock,
    ClockSet, Composed, MachineTime, SettingChange,
};

fn clock_in(info: &'static ZoneInfo<'static>, zone: &str, region: &str) -> CivilClock<'static> {
    CivilClock::new(
        info.zone(zone).expect("the fixture holds it"),
        info.region(region).as_ref(),
    )
}

fn london() -> CivilClock<'static> {
    clock_in(zones(), "Europe/London", "GB")
}

fn read(at: i64, state: WallTimeState, last_sync: Option<i64>) -> Clock {
    Clock::Read {
        reading: WallClockReading::new(Time64::from_secs(at), state),
        last_sync: last_sync.map(Time64::from_secs),
    }
}

fn date(year: i64, month: u32, day: u32) -> Date {
    Date::new(year, month, day).expect("a date")
}

fn at(hour: u32, minute: u32, second: u32) -> TimeOfDay {
    TimeOfDay::new(hour, minute, second).expect("a time")
}

const PLACE: &str = "London, United Kingdom";

#[test]
fn the_machines_time_settings_are_read_and_asked_for_in_the_registrys_spelling() {
    let config =
        SystemConfig::parse("time.network off\ntime.zone Asia/Kolkata\n").expect("a document");
    assert_eq!(
        MachineTime::of(&config),
        MachineTime {
            network: false,
            zone: ZoneName::new("Asia/Kolkata"),
        }
    );
    assert_eq!(
        MachineTime::of(&SystemConfig::default()),
        MachineTime {
            network: true,
            zone: None
        },
        "network time is on and the store's zone kept, by default"
    );
    assert_eq!(MachineTime::network_setting(false), ("time.network", "off"));
    assert_eq!(MachineTime::network_setting(true), ("time.network", "on"));
    assert_eq!(
        MachineTime::zone_setting(ZoneName::new("Europe/Paris").as_ref()),
        ("time.zone", "Europe/Paris")
    );
    assert_eq!(MachineTime::zone_setting(None), ("time.zone", "default"));
}

#[test]
fn a_network_set_clock_reads_to_the_second_with_when_and_the_next_change() {
    let now = utc(2026, 7, 1, 13, 2) + 9;
    let shown = readout(
        read(now, WallTimeState::Trusted, Some(utc(2026, 7, 1, 12, 58))),
        true,
        PLACE,
        &london(),
    );
    assert_eq!(shown.time, "14:02:09");
    assert_eq!(shown.date, "Wednesday 1 July 2026");
    assert_eq!(shown.zone, "London, United Kingdom \u{b7} BST, UTC+01:00");
    assert_eq!(
        shown.source,
        "Set automatically from the network today at 13:58."
    );
    assert_eq!(
        shown.change,
        "Clocks go back an hour on Sunday 25 October at 02:00."
    );
}

#[test]
fn when_the_network_last_set_the_clock_is_named_from_today() {
    let now = utc(2026, 7, 1, 13, 2);
    let source = |last: i64| {
        readout(
            read(now, WallTimeState::Trusted, Some(last)),
            true,
            PLACE,
            &london(),
        )
        .source
    };
    assert_eq!(
        source(utc(2026, 6, 30, 22, 30)),
        "Set automatically from the network yesterday at 23:30."
    );
    assert_eq!(
        source(utc(2026, 6, 27, 8, 0)),
        "Set automatically from the network on Saturday 27 June at 09:00."
    );
    assert_eq!(
        source(utc(2025, 12, 31, 12, 0)),
        "Set automatically from the network on Wednesday 31 December 2025 at 12:00."
    );
    assert_eq!(
        readout(
            read(now, WallTimeState::Trusted, None),
            true,
            PLACE,
            &london()
        )
        .source,
        "Set automatically from the network.",
        "no record says when"
    );
    assert_eq!(
        source(now + 3600),
        "Set automatically from the network.",
        "a record from the future says nothing it can be trusted for"
    );
}

#[test]
fn every_provenance_is_stated_and_an_unset_or_unread_clock_shows_no_reading() {
    let now = utc(2026, 7, 1, 13, 2);
    let source = |state| readout(read(now, state, None), false, PLACE, &london()).source;
    assert_eq!(
        source(WallTimeState::Firmware),
        "From the clock chip, read when the machine started."
    );
    assert_eq!(source(WallTimeState::Adjusted), "Set by hand.");

    let unset = readout(read(0, WallTimeState::Unset, None), true, PLACE, &london());
    assert_eq!(unset.time, "Not set");
    assert!(
        !unset.date.contains("1970"),
        "the placeholder is no reading"
    );
    assert_eq!(
        unset.source,
        "Waiting for a time server on the network to answer."
    );
    assert_eq!(
        readout(read(0, WallTimeState::Unset, None), false, PLACE, &london()).source,
        "Set the date and time below, or turn on network time."
    );
    let unread = readout(Clock::Unreadable, true, PLACE, &london());
    assert_eq!(unread.time, "Unknown");
    assert_eq!(unread.date, "The machine's clock could not be read.");
    assert_eq!(Clock::Unreadable.now(), None);
    assert_eq!(read(0, WallTimeState::Unset, None).now(), None);
    assert_eq!(
        read(now, WallTimeState::Adjusted, None).now(),
        Some(Time64::from_secs(now))
    );
}

#[test]
fn a_twelve_hour_region_reads_its_own_clock_and_a_fixed_zone_has_no_change() {
    let info = zones();
    let now = utc(2026, 7, 1, 13, 2) + 9;
    let shown = readout(
        read(now, WallTimeState::Adjusted, None),
        false,
        "New York, United States",
        &clock_in(info, "America/New_York", "US"),
    );
    assert_eq!(shown.time, "9:02:09 AM");
    assert_eq!(
        shown.change,
        "Clocks go back an hour on Sunday 1 November at 2:00 AM."
    );
    let kolkata = readout(
        read(now, WallTimeState::Adjusted, None),
        false,
        "Kolkata, India",
        &clock_in(info, "Asia/Kolkata", "IN"),
    );
    assert_eq!(kolkata.change, "No change of the clocks is scheduled here.");
    assert_eq!(kolkata.zone, "Kolkata, India \u{b7} IST, UTC+05:30");
}

#[test]
fn the_next_change_forward_is_named_with_the_time_the_clocks_leave() {
    let winter = utc(2026, 11, 1, 12, 0);
    let shown = readout(
        read(winter, WallTimeState::Adjusted, None),
        false,
        PLACE,
        &london(),
    );
    assert_eq!(
        shown.change,
        "Clocks go forward an hour on Sunday 28 March 2027 at 01:00."
    );
    let eve = utc(2026, 10, 24, 12, 0);
    assert_eq!(
        readout(
            read(eve, WallTimeState::Adjusted, None),
            false,
            PLACE,
            &london()
        )
        .change,
        "Clocks go back an hour tomorrow at 02:00."
    );
}

#[test]
fn a_zone_line_says_only_what_its_place_does_not() {
    let utc_offset = |utc_offset, abbreviation| ZoneOffset {
        utc_offset,
        abbreviation,
        daylight: false,
    };
    assert_eq!(zone_line("UTC", utc_offset(0, "UTC")), "UTC");
    assert_eq!(
        zone_line("Istanbul, Türkiye", utc_offset(3 * 3600, "+03")),
        "Istanbul, Türkiye \u{b7} UTC+03:00",
        "a numeric abbreviation repeats the offset"
    );
    assert_eq!(
        zone_line("EST", utc_offset(-5 * 3600, "EST")),
        "EST \u{b7} UTC-05:00"
    );
}

#[test]
fn a_hand_set_time_names_its_instant_in_the_zone() {
    let zone = london().zone();
    assert_eq!(
        compose(zone, date(2026, 7, 1), at(14, 2, 9), Disambiguation::Reject),
        Composed::At(Time64::from_secs(utc(2026, 7, 1, 13, 2) + 9))
    );
    assert_eq!(
        compose(
            zone,
            date(1969, 12, 31),
            at(23, 0, 0),
            Disambiguation::Reject
        ),
        Composed::At(Time64::from_secs(-3600)),
        "before 1970 is an ordinary date"
    );
}

#[test]
fn a_skipped_time_is_refused_with_the_change_that_skips_it() {
    let civil = london();
    let (day, time) = (date(2026, 3, 29), at(1, 30, 0));
    let Composed::Skipped { from, to } = compose(civil.zone(), day, time, Disambiguation::Reject)
    else {
        panic!("01:30 is skipped that morning");
    };
    assert_eq!(
        skipped_reason(day, time, &from, &to, &civil),
        "01:30:00 does not happen on Sunday 29 March 2026: the clocks go forward from 01:00 \
         to 02:00."
    );
}

#[test]
fn a_twice_passed_time_asks_which_it_means_and_then_names_that_one() {
    let civil = london();
    let (day, time) = (date(2026, 10, 25), at(1, 30, 0));
    let Composed::Repeated {
        earlier,
        later,
        at: back,
    } = compose(civil.zone(), day, time, Disambiguation::Reject)
    else {
        panic!("01:30 comes round twice that morning");
    };
    let (headline, detail, answers) =
        repeated_question(day, time, (&earlier, &later, &back), &civil);
    assert_eq!(headline, "01:30:00 happens twice on Sunday 25 October 2026");
    assert_eq!(
        detail,
        "The clocks go back an hour at 02:00, so 01:30:00 comes round twice: once before the \
         change and once after. Which do you mean?"
    );
    assert_eq!(answers, ["Before: BST, UTC+01:00", "After: GMT, UTC+00:00"]);
    assert!(answers.iter().all(|label| label.len() <= 32));
    assert_eq!(
        compose(civil.zone(), day, time, Disambiguation::Earlier),
        Composed::At(Time64::from_secs(utc(2026, 10, 25, 0, 30)))
    );
    assert_eq!(
        compose(civil.zone(), day, time, Disambiguation::Later),
        Composed::At(Time64::from_secs(utc(2026, 10, 25, 1, 30)))
    );
}

#[test]
fn a_time_outside_the_plausible_window_is_confirmed_first() {
    let civil = london();
    let (headline, detail) =
        implausible_question(Time64::from_secs(utc(1990, 1, 1, 12, 0)), &civil)
            .expect("1990 is before this system was made");
    assert_eq!(headline, "Set the clock to Monday 1 January 1990?");
    assert!(detail.starts_with("That is before this system was made"));
    let (_, far) = implausible_question(Time64::from_secs(utc(2126, 1, 3, 0, 0)), &civil)
        .expect("a century after this system was made is no working clock's reading");
    assert!(far.starts_with("That is more than a century after this system was made"));
    assert_eq!(
        implausible_question(Time64::from_secs(utc(2125, 12, 31, 0, 0)), &civil),
        None,
        "the century is counted from the system's making, not from the reading"
    );
    assert_eq!(
        implausible_question(Time64::from_secs(utc(2026, 10, 19, 9, 0)), &civil),
        None
    );
}

#[test]
fn a_clock_step_states_its_refusal_and_its_chips() {
    let refused = ClockSet {
        clock: Err(Errno::PermissionDenied),
        chip: None,
    };
    assert_eq!(
        refused.refusal(),
        Some((
            "The clock was not set",
            alloc::string::String::from("This account may not set the clock. Nothing was changed.")
        ))
    );
    assert_eq!(refused.note(), None);
    let other = ClockSet {
        clock: Err(Errno::OutOfRange),
        chip: None,
    };
    assert!(other
        .refusal()
        .is_some_and(|(_, detail)| detail.contains("refused it")));
    let chipless = ClockSet {
        clock: Ok(()),
        chip: Some(Err(Errno::NotFound)),
    };
    assert_eq!(chipless.refusal(), None);
    assert!(chipless
        .note()
        .is_some_and(|note| note.contains("no clock chip")));
    let stubborn = ClockSet {
        clock: Ok(()),
        chip: Some(Err(Errno::PermissionDenied)),
    };
    assert!(stubborn
        .note()
        .is_some_and(|note| note.contains("clock chip refused it")));
    let kept = ClockSet {
        clock: Ok(()),
        chip: Some(Ok(())),
    };
    assert_eq!((kept.refusal(), kept.note()), (None, None));
}

#[test]
fn a_settings_change_is_asked_for_and_refused_in_its_own_words() {
    assert_eq!(
        SettingChange::Network(false).setting(),
        ("time.network", "off")
    );
    assert_eq!(
        SettingChange::Zone(ZoneName::new("Asia/Kolkata")).setting(),
        ("time.zone", "Asia/Kolkata")
    );
    assert_eq!(
        SettingChange::Zone(None).setting(),
        ("time.zone", "default")
    );
    let (headline, detail) = SettingChange::Network(true).refusal(Errno::PermissionDenied);
    assert_eq!(headline, "Network time was not turned on");
    assert_eq!(
        detail,
        "This account may not change the machine's settings."
    );
    assert_eq!(
        SettingChange::Network(false).refusal(Errno::NotFound),
        (
            "Network time was not turned off",
            alloc::string::String::from("The settings service is not running.")
        )
    );
    let (headline, detail) = SettingChange::Zone(None).refusal(Errno::BadMagic);
    assert_eq!(headline, "The time zone was not changed");
    assert!(detail.contains("could not be read"));
}
