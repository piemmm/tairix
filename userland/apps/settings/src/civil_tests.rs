//! Host tests for the Language & Region choices and the Date & Time readings,
//! against the fixture store of real zones and regions.

use tairix_abi::time::Time64;
use tairix_tz::CivilClock;
use tairix_wallpaper::{DesktopSettings, RegionCode, ZoneName};

use super::{
    choose_region, choose_zone, reading, region_choices, written_example, zone_choices,
    zone_reading, CivilChoices,
};
use crate::test_support::zones;

fn choices() -> CivilChoices {
    CivilChoices::of(zones())
}

fn held(zone: Option<&str>, region: Option<&str>) -> DesktopSettings {
    DesktopSettings {
        time_zone: zone.and_then(ZoneName::new),
        region: region.and_then(RegionCode::new),
        ..DesktopSettings::default()
    }
}

#[test]
fn the_regions_are_offered_by_name_after_the_systems_own() {
    let civil = choices();
    let (labels, at) = region_choices(&held(None, Some("IE")), Some(&civil));
    assert_eq!(
        labels,
        [
            "Default (United Kingdom)",
            "India",
            "Ireland",
            "United Kingdom",
            "United States"
        ]
    );
    assert_eq!(at, 2, "the held region is the one selected");
    let mut settings = held(None, None);
    assert!(choose_region(4, &mut settings, Some(&civil)));
    assert_eq!(settings.region, RegionCode::new("US"));
    assert!(choose_region(0, &mut settings, Some(&civil)));
    assert_eq!(settings.region, None, "the system's own");
    assert!(
        !choose_region(5, &mut settings, Some(&civil)),
        "fails closed"
    );
}

#[test]
fn the_zones_are_offered_by_city_after_the_systems_own() {
    let civil = choices();
    let (labels, at) = zone_choices(&held(None, None), Some(&civil));
    assert_eq!(
        labels,
        [
            "Default (London, United Kingdom)",
            "Belfast, United Kingdom",
            "Dublin, Ireland",
            "Kolkata, India",
            "London, United Kingdom",
            "New York, United States"
        ]
    );
    assert_eq!(at, 0);
    let mut settings = held(None, None);
    assert!(choose_zone(5, &mut settings, Some(&civil)));
    assert_eq!(settings.time_zone, ZoneName::new("America/New_York"));
    assert_eq!(zone_choices(&settings, Some(&civil)).1, 5);
}

#[test]
fn a_held_choice_the_store_lacks_is_still_offered_by_its_own_name() {
    let civil = choices();
    let settings = held(Some("Mars/Olympus"), Some("ZZ"));
    let (labels, at) = region_choices(&settings, Some(&civil));
    assert_eq!((labels[1].as_str(), at), ("ZZ", 1));
    let (labels, at) = zone_choices(&settings, Some(&civil));
    assert_eq!((labels[1].as_str(), at), ("Mars/Olympus", 1));
    assert_eq!(labels.len(), 7, "the held value, then every place");
    let mut kept = settings.clone();
    assert!(choose_zone(1, &mut kept, Some(&civil)));
    assert_eq!(
        kept.time_zone, settings.time_zone,
        "choosing it changes nothing"
    );
    assert!(choose_zone(2, &mut kept, Some(&civil)));
    assert_eq!(kept.time_zone, ZoneName::new("Europe/Belfast"));
    let listed = held(Some("Europe/Dublin"), None);
    assert_eq!(zone_choices(&listed, Some(&civil)).0.len(), 6, "not twice");
}

#[test]
fn with_no_store_only_the_default_and_the_held_choice_are_offered() {
    assert_eq!(region_choices(&held(None, None), None).0, ["Default"]);
    let settings = held(Some("Europe/London"), None);
    let (labels, at) = zone_choices(&settings, None);
    assert_eq!(labels, ["Default", "Europe/London"]);
    assert_eq!(at, 1);
}

#[test]
fn names_order_without_case_or_accent() {
    let mut names = ["Zambia", "Åland Islands", "albania", "Curaçao", "Cuba"];
    names.sort_by(|a, b| super::name_order(a, b));
    assert_eq!(
        names,
        ["Åland Islands", "albania", "Cuba", "Curaçao", "Zambia"]
    );
}

#[test]
fn a_regions_example_shows_its_field_order_and_its_clock() {
    let info = zones();
    let british = CivilClock::new(info.default_zone(), info.region("GB").as_ref());
    assert_eq!(
        written_example(&british),
        "Dates are written 31/12/2026 and times 23:30."
    );
    let american = CivilClock::new(info.default_zone(), info.region("US").as_ref());
    assert_eq!(
        written_example(&american),
        "Dates are written 12/31/2026 and times 11:30 PM."
    );
}

#[test]
fn the_clock_reads_the_day_the_time_and_the_zone() {
    let info = zones();
    // 2026-10-18 13:05 UTC.
    let now = Time64::from_secs(1_792_328_700);
    let london = CivilClock::new(info.default_zone(), info.default_region().as_ref());
    assert_eq!(reading(now, &london), "Sunday 18 October 2026 14:05");
    assert_eq!(
        zone_reading(Some(now), &london, Some(info)),
        "London, United Kingdom, BST (UTC+01:00)"
    );
    assert_eq!(
        zone_reading(None, &london, Some(info)),
        "London, United Kingdom",
        "no time is held"
    );
    let new_york = CivilClock::new(
        info.zone("America/New_York").expect("New York"),
        info.region("US").as_ref(),
    );
    assert_eq!(
        zone_reading(Some(now), &new_york, Some(info)),
        "New York, United States, EDT (UTC-04:00)"
    );
    assert_eq!(zone_reading(Some(now), &CivilClock::UTC, None), "UTC");
}
