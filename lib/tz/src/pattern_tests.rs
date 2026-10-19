//! Host tests of the date and time patterns: what each region's spelling
//! renders, and the shapes no pattern may take.

use alloc::string::String;

use tairix_abi::time::CivilTime;

use super::{Pattern, ISO_DATE, ISO_TIME};

const AFTERNOON: CivilTime = CivilTime {
    year: 2026,
    month: 10,
    day: 8,
    hour: 14,
    minute: 5,
    second: 9,
};

fn spelled(pattern: Pattern<'_>, civil: &CivilTime) -> String {
    let mut out = String::new();
    pattern
        .write(civil, &mut out)
        .expect("a String takes any write");
    out
}

#[test]
fn each_regions_date_reads_in_its_own_order_padding_and_separators() {
    let date = |text| spelled(Pattern::date(text).expect("a date pattern"), &AFTERNOON);
    assert_eq!(date("dd/MM/y"), "08/10/2026", "the United Kingdom");
    assert_eq!(date("M/d/y"), "10/8/2026", "the United States");
    assert_eq!(date("d.M.y"), "8.10.2026", "Germany");
    assert_eq!(date("y. M. d."), "2026. 10. 8.", "South Korea");
    assert_eq!(date("y-MM-dd"), "2026-10-08", "Sweden");
    assert_eq!(spelled(ISO_DATE, &AFTERNOON), "2026-10-08");
}

#[test]
fn a_time_to_the_second_sets_its_seconds_off_as_the_minutes_are() {
    let time = |text| {
        let mut out = String::new();
        Pattern::time(text)
            .expect("a time pattern")
            .write_with_seconds(&AFTERNOON, &mut out)
            .expect("a String takes any write");
        out
    };
    assert_eq!(time("HH:mm"), "14:05:09");
    assert_eq!(time("H.mm"), "14.05.09", "Finland");
    assert_eq!(time("h:mm a"), "2:05:09 PM");
    assert_eq!(time("a h:mm"), "PM 2:05:09", "South Korea");
    assert_eq!(
        spelled(ISO_TIME, &AFTERNOON),
        "14:05",
        "plain writing keeps none"
    );
}

#[test]
fn each_regions_time_reads_on_its_own_clock() {
    let time =
        |text, civil: &CivilTime| spelled(Pattern::time(text).expect("a time pattern"), civil);
    assert_eq!(time("HH:mm", &AFTERNOON), "14:05");
    assert_eq!(time("H.mm", &AFTERNOON), "14.05", "Finland");
    assert_eq!(time("h:mm a", &AFTERNOON), "2:05 PM");
    assert_eq!(time("a h:mm", &AFTERNOON), "PM 2:05", "South Korea");
    let after_midnight = CivilTime {
        hour: 0,
        minute: 30,
        ..AFTERNOON
    };
    assert_eq!(time("h:mm a", &after_midnight), "12:30 AM");
    assert_eq!(time("H:mm", &after_midnight), "0:30");
    let noon = CivilTime {
        hour: 12,
        minute: 0,
        ..AFTERNOON
    };
    assert_eq!(time("h:mm a", &noon), "12:00 PM");
    assert_eq!(spelled(ISO_TIME, &noon), "12:00");
}

#[test]
fn a_date_pattern_holds_a_day_a_month_and_a_year_and_nothing_else() {
    for refused in [
        "dd/MM",
        "MM/y",
        "dd/MM/yy",
        "ddd/MM/y",
        "dd/MM/y HH:mm",
        "",
        "y",
    ] {
        assert_eq!(Pattern::date(refused), None, "{refused:?}");
    }
    assert_eq!(Pattern::date("dd/dd/y"), None, "a field twice");
    assert_eq!(Pattern::date("HH:mm"), None, "a time is no date");
}

#[test]
fn a_time_pattern_holds_an_hour_and_minutes_and_a_period_only_on_a_12_hour_clock() {
    for refused in [
        "h:mm",
        "HH:mm a",
        "HH",
        "HH:m",
        "HH:mm:ss",
        "H:mm y",
        "hh:mm a a",
    ] {
        assert_eq!(Pattern::time(refused), None, "{refused:?}");
    }
    assert!(Pattern::time("hh:mm a").is_some());
}

#[test]
fn a_literal_is_ascii_punctuation_and_spaces_only() {
    assert_eq!(Pattern::date("d\u{200f}/M\u{200f}/y"), None, "a bidi mark");
    assert_eq!(
        Pattern::date("d.MM.y \u{433}."),
        None,
        "a script's own letters"
    );
    assert_eq!(Pattern::date("d de M de y"), None, "words are letters");
    assert_eq!(Pattern::date("d/M/y\t"), None, "a control character");
    assert!(Pattern::date("dd  /  MM  /  y").is_some());
    assert_eq!(Pattern::date("dd  /  MM  /  y  "), None, "past the bound");
}

/// ISO 8601 writes a year in at least four digits after its sign.
#[test]
fn the_iso_date_writes_a_year_in_four_digits_after_its_sign() {
    let year = |year| spelled(ISO_DATE, &CivilTime { year, ..AFTERNOON });
    assert_eq!(year(999), "0999-10-08");
    assert_eq!(year(-44), "-0044-10-08");
    assert_eq!(year(12_345), "12345-10-08");
    assert_eq!(
        Pattern::date("yyyy-MM-dd").map(|p| p.as_str()),
        Some("yyyy-MM-dd")
    );
    assert_eq!(
        Pattern::date("yy-MM-dd"),
        None,
        "two digits lose the century"
    );
}

/// Fields a caller built out of range write without panicking.
#[test]
fn a_twelve_hour_clock_reads_any_hour_it_is_given() {
    let late = CivilTime {
        hour: u32::MAX,
        ..AFTERNOON
    };
    let time = Pattern::time("h:mm a").expect("a time pattern");
    assert_eq!(spelled(time, &late), "3:05 PM");
}
