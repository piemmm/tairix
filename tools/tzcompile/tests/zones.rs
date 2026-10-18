//! The zones whose rules are hardest to get right, read through the compiled
//! store at stated instants: what each must say, written out, beside the
//! oracle's comparison of every zone with `zic`.

use std::sync::OnceLock;

use tairix_abi::time::{days_from_civil, CivilTime, Time64};
use tairix_tz::{Disambiguation, LocalError, ZoneInfo};

const HOUR: i32 = 3600;

fn store() -> &'static [u8] {
    static STORE: OnceLock<Vec<u8>> = OnceLock::new();
    STORE.get_or_init(|| tairix_tzcompile::compile().expect("the store compiles"))
}

fn utc(year: i64, month: u32, day: u32, hour: i64, minute: i64) -> i64 {
    days_from_civil(year, month, day).expect("a representable date") * 86_400
        + hour * 3600
        + minute * 60
}

/// What `zone` reads at the UT time given.
fn reads(zone: &str, at: i64) -> (i32, String, bool) {
    let info = ZoneInfo::parse(store()).expect("the store parses");
    let offset = info
        .zone(zone)
        .unwrap_or_else(|| panic!("{zone} is stored"))
        .offset_at(Time64::from_secs(at));
    (
        offset.utc_offset,
        String::from(offset.abbreviation),
        offset.daylight,
    )
}

fn reading(offset: i32, abbreviation: &str, daylight: bool) -> (i32, String, bool) {
    (offset, String::from(abbreviation), daylight)
}

#[test]
fn london_keeps_local_mean_time_until_railway_time_then_the_european_rules() {
    assert_eq!(
        reads("Europe/London", utc(1800, 6, 1, 12, 0)),
        reading(-75, "LMT", false),
        "before 1847"
    );
    assert_eq!(
        reads("Europe/London", utc(1900, 6, 1, 12, 0)),
        reading(0, "GMT", false)
    );
    assert_eq!(
        reads("Europe/London", utc(1941, 6, 1, 12, 0)),
        reading(2 * HOUR, "BDST", true),
        "double summer time"
    );
    assert_eq!(
        reads("Europe/London", utc(1970, 1, 15, 12, 0)),
        reading(HOUR, "BST", false),
        "British Standard Time, 1968 to 1971"
    );
    let spring = utc(2026, 3, 29, 1, 0);
    assert_eq!(reads("Europe/London", spring - 1), reading(0, "GMT", false));
    assert_eq!(reads("Europe/London", spring), reading(HOUR, "BST", true));
    assert_eq!(
        reads("Europe/London", utc(2087, 10, 26, 1, 0)),
        reading(0, "GMT", false),
        "the last Sunday of October 2087, at one o'clock UT"
    );
}

#[test]
fn dublin_counts_winter_as_its_saving() {
    assert_eq!(
        reads("Europe/Dublin", utc(2026, 1, 15, 12, 0)),
        reading(0, "GMT", true)
    );
    assert_eq!(
        reads("Europe/Dublin", utc(2026, 7, 15, 12, 0)),
        reading(HOUR, "IST", false)
    );
}

#[test]
fn half_and_three_quarter_hour_zones_keep_their_fractions() {
    assert_eq!(
        reads("Asia/Kolkata", utc(2026, 1, 1, 0, 0)),
        reading(19_800, "IST", false)
    );
    assert_eq!(
        reads("Asia/Kathmandu", utc(2026, 1, 1, 0, 0)),
        reading(20_700, "+0545", false)
    );
    assert_eq!(
        reads("Australia/Lord_Howe", utc(2026, 7, 1, 0, 0)),
        reading(37_800, "+1030", false)
    );
    assert_eq!(
        reads("Australia/Lord_Howe", utc(2026, 12, 1, 0, 0)),
        reading(39_600, "+11", true),
        "half an hour of saving"
    );
}

#[test]
fn southern_summer_spans_the_new_year() {
    assert_eq!(
        reads("Australia/Sydney", utc(2026, 1, 1, 0, 0)),
        reading(11 * HOUR, "AEDT", true)
    );
    assert_eq!(
        reads("Australia/Sydney", utc(2026, 7, 1, 0, 0)),
        reading(10 * HOUR, "AEST", false)
    );
    // The first Sunday of October 2040 is the 7th: 02:00 standard is 16:00 UT
    // the day before.
    let spring = utc(2040, 10, 6, 16, 0);
    assert_eq!(
        reads("Australia/Sydney", spring - 1),
        reading(10 * HOUR, "AEST", false)
    );
    assert_eq!(
        reads("Australia/Sydney", spring),
        reading(11 * HOUR, "AEDT", true)
    );
}

#[test]
fn a_zone_that_changed_its_rules_more_than_once_reads_each_era() {
    // America/Indiana/Indianapolis: Central time until 1955, Eastern without
    // daylight saving from 1971, the US rules again from 2006.
    let zone = "America/Indiana/Indianapolis";
    assert_eq!(
        reads(zone, utc(1950, 1, 15, 12, 0)),
        reading(-6 * HOUR, "CST", false)
    );
    assert_eq!(
        reads(zone, utc(1990, 7, 15, 12, 0)),
        reading(-5 * HOUR, "EST", false)
    );
    assert_eq!(
        reads(zone, utc(2026, 7, 15, 12, 0)),
        reading(-4 * HOUR, "EDT", true)
    );
}

#[test]
fn a_local_time_in_the_gap_or_the_overlap_is_named_as_such() {
    let info = ZoneInfo::parse(store()).expect("the store parses");
    let london = info.zone("Europe/London").expect("London");
    let civil = |day, hour| CivilTime {
        year: 2026,
        month: if day == 29 { 3 } else { 10 },
        day,
        hour,
        minute: 30,
        second: 0,
    };
    assert_eq!(
        london.instant_from_local(&civil(29, 1), Disambiguation::Earlier),
        Err(LocalError::Gap)
    );
    assert_eq!(
        london.instant_from_local(&civil(25, 1), Disambiguation::Reject),
        Err(LocalError::Overlap)
    );
    assert_eq!(
        london.instant_from_local(&civil(25, 1), Disambiguation::Later),
        Ok(Time64::from_secs(utc(2026, 10, 25, 1, 30)))
    );
}

#[test]
fn the_shipped_defaults_are_london_and_the_united_kingdom() {
    let info = ZoneInfo::parse(store()).expect("the store parses");
    assert_eq!(info.default_zone().name(), "Europe/London");
    let region = info.default_region().expect("a default region");
    assert_eq!(
        (
            region.code,
            region.name,
            region.date.as_str(),
            region.time.as_str()
        ),
        ("GB", "United Kingdom", "dd/MM/y", "HH:mm")
    );
    assert_eq!(info.regions().count(), 249);
    assert_eq!(info.places().count(), 418);
    let accra = info
        .places()
        .find(|place| place.region == "GH")
        .expect("Ghana's place");
    assert_eq!(
        accra.zone.name(),
        "Africa/Accra",
        "a place listed by an alias keeps its name"
    );
}

#[test]
fn the_store_stays_small_enough_to_keep_resident() {
    assert!(store().len() < 256 * 1024, "{} bytes", store().len());
}
