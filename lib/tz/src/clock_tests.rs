//! Host tests of resolution and the civil clock: which zone and region apply,
//! and what a reading reads as.

use alloc::string::String;

use tairix_abi::time::Time64;

use crate::testkit::{store, utc};
use crate::{resolve_region, resolve_zone, CivilClock, Decided, ZoneInfo};

#[test]
fn the_first_preference_the_store_holds_decides_the_zone() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let (zone, decided) = resolve_zone(
        Some(&info),
        &["Mars/Olympus", "America/New_York", "Asia/Kolkata"],
    );
    assert_eq!(
        (zone.name(), decided),
        ("America/New_York", Decided::Preference(1))
    );
    let (zone, decided) = resolve_zone(Some(&info), &["Europe/Belfast"]);
    assert_eq!(
        (zone.name(), decided),
        ("Europe/Belfast", Decided::Preference(0)),
        "an alias, by its own name"
    );
    let (zone, decided) = resolve_zone(Some(&info), &["Mars/Olympus"]);
    assert_eq!(
        (zone.name(), decided),
        ("Europe/London", Decided::StoreDefault)
    );
    let (zone, decided) = resolve_zone(None, &["America/New_York"]);
    assert_eq!((zone.name(), decided), ("UTC", Decided::NoStore));
}

#[test]
fn the_first_preference_the_store_holds_decides_the_region() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let (region, decided) = resolve_region(Some(&info), &["XX", "US"]);
    assert_eq!(
        (region.map(|region| region.code), decided),
        (Some("US"), Decided::Preference(1))
    );
    let (region, decided) = resolve_region(Some(&info), &[]);
    assert_eq!(
        (region.map(|region| region.code), decided),
        (Some("GB"), Decided::StoreDefault)
    );
    assert_eq!(resolve_region(None, &["GB"]), (None, Decided::NoStore));
}

fn reading(clock: &CivilClock<'_>, at: i64) -> (String, String) {
    let local = clock.local(Time64::from_secs(at));
    let (mut date, mut time) = (String::new(), String::new());
    clock
        .write_date(&local, &mut date)
        .expect("a String takes any write");
    clock
        .write_time(&local, &mut time)
        .expect("a String takes any write");
    (date, time)
}

#[test]
fn a_clock_reads_its_zone_in_its_regions_conventions() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let afternoon = utc(2026, 10, 18, 13, 5);
    let london = CivilClock::new(info.default_zone(), info.default_region().as_ref());
    assert_eq!(
        reading(&london, afternoon),
        (String::from("18/10/2026"), String::from("14:05")),
        "British summer time, the British way"
    );
    let us = info.region("US");
    let new_york = CivilClock::new(
        info.zone("America/New_York").expect("New York"),
        us.as_ref(),
    );
    assert_eq!(
        reading(&new_york, afternoon),
        (String::from("10/18/2026"), String::from("9:05 AM"))
    );
    assert_eq!(
        reading(&CivilClock::UTC, afternoon),
        (String::from("2026-10-18"), String::from("13:05")),
        "no store: UTC, the ISO way"
    );
    let gb = info.region("GB");
    let kolkata_in_gb = CivilClock::new(info.zone("Asia/Kolkata").expect("Kolkata"), gb.as_ref());
    assert_eq!(
        reading(&kolkata_in_gb, afternoon),
        (String::from("18/10/2026"), String::from("18:35")),
        "the zone and the conventions are each their own choice"
    );
}
