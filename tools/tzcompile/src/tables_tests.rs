//! Host tests of the two tables' readers.

use super::{places, regions, TableError};
use tairix_tz::Weekday;

#[test]
fn a_zone_table_row_places_a_zone_in_a_region() {
    let text = "# comment\n\nGB\t+513030-0000731\tEurope/London\nUS\t+404251-0740023\tAmerica/New_York\tEastern (most areas)\n";
    let rows = places(text).expect("the table reads");
    let read: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|place| {
            (
                place.region.as_str(),
                place.zone.as_str(),
                place.comment.as_str(),
            )
        })
        .collect();
    assert_eq!(
        read,
        [
            ("GB", "Europe/London", ""),
            ("US", "America/New_York", "Eastern (most areas)")
        ]
    );
    assert_eq!(
        places("GB\tEurope/London\n").map(|_| ()),
        Err(TableError {
            line: 1,
            reason: "a zone table row is a code, coordinates, a zone and a comment",
        })
    );
}

#[test]
fn a_region_row_states_its_name_patterns_and_week() {
    let rows = regions(
        "# code\tname\tdate\ttime\tweek\nGB\tUnited Kingdom\tdd/MM/y\tHH:mm\tmon\n\
         US\tUnited States\tM/d/y\th:mm a\tsun\n",
    )
    .expect("the table reads");
    let [gb, us] = &rows[..] else {
        panic!("two regions");
    };
    assert_eq!(
        (
            gb.code.as_str(),
            gb.name.as_str(),
            gb.date.as_str(),
            gb.time.as_str(),
            gb.first_weekday
        ),
        ("GB", "United Kingdom", "dd/MM/y", "HH:mm", Weekday::Monday)
    );
    assert_eq!(us.first_weekday, Weekday::Sunday);
    assert_eq!(
        regions("\n\nGB\tUnited Kingdom\tdd/MM/y\tHH:mm\n").map(|_| ()),
        Err(TableError {
            line: 3,
            reason: "a region row is a code, a name, a date, a time and a week",
        })
    );
    assert_eq!(
        regions("GB\tUnited Kingdom\tdd/MM/y\tHH:mm\tMon\n").map(|_| ()),
        Err(TableError {
            line: 1,
            reason: "a week begins on mon, tue, wed, thu, fri, sat or sun",
        })
    );
}
