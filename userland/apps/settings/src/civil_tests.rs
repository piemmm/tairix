//! Host tests for the Language & Region choices and the Date & Time readings,
//! against the fixture store of real zones and regions.

use tairix_abi::time::Time64;
use tairix_tz::CivilClock;
use tairix_wallpaper::{DesktopSettings, RegionCode, ZoneName};

use super::{
    choose_region, reading, refresh_zone_readings, region_choices, written_example, zone_held,
    zone_of_row, zone_reading, zone_rows, CivilChoices, ZoneChoice,
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
fn the_zone_chooser_lists_the_machines_zone_then_every_place_by_city() {
    let civil = choices();
    let now = Time64::from_secs(tairix_tz::testkit::utc(2026, 7, 1, 13, 2));
    let (rows, at) = zone_rows(&held(None, None), &civil, None, Some(now));
    let names: alloc::vec::Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "This machine's zone",
            "Belfast",
            "Dublin",
            "Kolkata",
            "London",
            "New York"
        ]
    );
    assert_eq!(at, Some(0), "following the machine holds the first row");
    assert_eq!(rows[0].region, "London, United Kingdom");
    assert_eq!(rows[0].reading, "14:02 \u{b7} UTC+01:00");
    assert_eq!(rows[5].key, "America/New_York");
    let (_, at) = zone_rows(&held(Some("Asia/Kolkata"), None), &civil, None, None);
    assert_eq!(at, Some(3));
}

/// The readings are the time there now, so a chooser left open has them
/// rewritten as each minute turns: the machine's own row with the places.
#[test]
fn a_turned_minute_rewrites_the_machines_row_and_every_place() {
    use tairix_controls::zone::{self, Minute};
    use tairix_theme::Theme;

    let civil = choices();
    let at = |minute| Time64::from_secs(tairix_tz::testkit::utc(2026, 7, 1, 13, minute));
    let machine = ZoneName::new("Asia/Kolkata");
    let (rows, held_row) = zone_rows(&held(None, None), &civil, machine.as_ref(), Some(at(2)));
    let mut dialog = zone::dialog(rows, held_row);
    let mut written = Minute::of(Some(at(2)));
    let theme = Theme::dark();
    let (width, height) = dialog.opening_extent(tairix_geometry::Scale::ONE, &theme);
    let frame = (
        tairix_geometry::Rect::new(0, 0, width, height),
        tairix_geometry::Scale::ONE,
        &theme,
    );
    let mut damage = tairix_controls::damage::sink();
    refresh_zone_readings(
        &civil,
        machine.as_ref(),
        (&mut dialog, &mut written),
        at(3),
        frame,
        &mut damage,
    );
    let rows = dialog.chooser().rows();
    assert_eq!(
        rows[0].reading, "18:33 \u{b7} UTC+05:30",
        "the machine's zone"
    );
    assert_eq!(rows[4].name, "London");
    assert_eq!(rows[4].reading, "14:03 \u{b7} UTC+01:00");
    assert!(!damage.is_empty());
}

#[test]
fn a_chosen_row_names_the_zone_it_sets_and_the_first_follows_the_machine() {
    let civil = choices();
    assert_eq!(zone_of_row(&civil, 0), Some(ZoneChoice::Machine));
    assert_eq!(zone_of_row(&civil, 0).and_then(ZoneChoice::setting), None);
    let new_york = ZoneName::new("America/New_York").expect("a zone name");
    assert_eq!(zone_of_row(&civil, 5), Some(ZoneChoice::Own(new_york)));
    assert_eq!(
        zone_of_row(&civil, 5).and_then(ZoneChoice::setting),
        Some(new_york)
    );
    assert_eq!(zone_of_row(&civil, 6), None, "fails closed past the list");
}

#[test]
fn the_row_says_which_zone_its_clocks_read() {
    let civil = choices();
    assert_eq!(
        zone_held(&held(None, None), Some(&civil), None),
        "London, United Kingdom (this machine's zone)"
    );
    let kolkata = ZoneName::new("Asia/Kolkata");
    assert_eq!(
        zone_held(&held(None, None), Some(&civil), kolkata.as_ref()),
        "Kolkata, India (this machine's zone)"
    );
    let lost = ZoneName::new("Mars/Olympus");
    assert_eq!(
        zone_held(&held(None, None), Some(&civil), lost.as_ref()),
        "London, United Kingdom (this machine's zone)",
        "a machine zone the store lacks reads the store's own"
    );
    assert_eq!(
        zone_held(&held(Some("Europe/Dublin"), None), Some(&civil), None),
        "Dublin, Ireland"
    );
    assert_eq!(
        zone_held(&held(Some("Mars/Olympus"), None), Some(&civil), None),
        "Mars/Olympus",
        "a held zone the store lacks keeps its own name"
    );
    let (_, held_row) = zone_rows(&held(Some("Mars/Olympus"), None), &civil, None, None);
    assert_eq!(held_row, None, "and no row is highlighted for it");
}

#[test]
fn a_held_region_the_store_lacks_is_still_offered_by_its_own_name() {
    let civil = choices();
    let settings = held(Some("Mars/Olympus"), Some("ZZ"));
    let (labels, at) = region_choices(&settings, Some(&civil));
    assert_eq!((labels[1].as_str(), at), ("ZZ", 1));
}

#[test]
fn with_no_store_only_the_default_region_is_offered_and_the_zone_held_is_named() {
    assert_eq!(region_choices(&held(None, None), None).0, ["Default"]);
    assert_eq!(
        zone_held(&held(Some("Europe/London"), None), None, None),
        "Europe/London"
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
