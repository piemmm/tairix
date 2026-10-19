//! The places a store lists: by city, told apart by region, and read at an
//! instant in a region's own clock.

use alloc::vec::Vec;

use tairix_abi::time::Time64;
use tairix_abi::zone::ZoneName;

use super::{label, Listing};
use crate::testkit::{store, utc};
use crate::ZoneInfo;

fn zone(name: &str) -> ZoneName {
    ZoneName::new(name).expect("a zone name")
}

#[test]
fn places_are_listed_by_city_and_told_apart_by_region() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("the fixture parses");
    let listing = Listing::of(&info, info.region("GB"));
    let cities: Vec<&str> = listing
        .entries()
        .iter()
        .map(|entry| entry.city.as_str())
        .collect();
    assert_eq!(
        cities,
        ["Belfast", "Dublin", "Kolkata", "London", "New York"]
    );
    let new_york = &listing.entries()[4];
    assert_eq!(new_york.region, "United States \u{b7} Eastern (most areas)");
    assert_eq!(new_york.zone, zone("America/New_York"));
    assert_eq!(listing.zone(4), Some(zone("America/New_York")));
    assert_eq!(listing.zone(5), None);
    assert_eq!(listing.index_of(&zone("Europe/London")), Some(3));
    assert_eq!(listing.index_of(&zone("Europe/Atlantis")), None);
}

#[test]
fn a_reading_is_the_local_time_there_in_the_regions_clock_and_its_offset() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("the fixture parses");
    let summer = Time64::from_secs(utc(2026, 7, 1, 13, 2));
    let british = Listing::of(&info, info.region("GB"));
    assert_eq!(
        british.reading(&zone("Europe/London"), summer),
        "14:02 \u{b7} UTC+01:00"
    );
    let american = Listing::of(&info, info.region("US"));
    assert_eq!(
        american.reading(&zone("America/New_York"), summer),
        "9:02 AM \u{b7} UTC-04:00"
    );
    assert_eq!(
        american.reading(&zone("Asia/Kolkata"), summer),
        "6:32 PM \u{b7} UTC+05:30"
    );
    assert_eq!(american.reading(&zone("Europe/Atlantis"), summer), "");
}

#[test]
fn a_zone_is_labelled_by_its_place_or_by_its_name_where_it_has_none() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("the fixture parses");
    assert_eq!(label(&info, "Europe/London"), "London, United Kingdom");
    assert_eq!(label(&info, "UTC"), "UTC");
}
