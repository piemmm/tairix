//! Host tests of the two tables' readers.

use super::{places, regions, TableError};

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
fn a_region_row_states_its_name_and_patterns() {
    let rows = regions("# code\tname\tdate\ttime\nGB\tUnited Kingdom\tdd/MM/y\tHH:mm\n")
        .expect("the table reads");
    let [region] = &rows[..] else {
        panic!("one region");
    };
    assert_eq!(
        (
            region.code.as_str(),
            region.name.as_str(),
            region.date.as_str(),
            region.time.as_str()
        ),
        ("GB", "United Kingdom", "dd/MM/y", "HH:mm")
    );
    assert_eq!(
        regions("\n\nGB\tUnited Kingdom\tdd/MM/y\n").map(|_| ()),
        Err(TableError {
            line: 3,
            reason: "a region row is a code, a name, a date and a time",
        })
    );
}
