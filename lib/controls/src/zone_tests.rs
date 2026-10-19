//! The time-zone dialog: its rows over a zone listing, its title and
//! highlight, and readings rewritten once a minute has turned, past any rows
//! of the owner's own.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;
use tairix_geometry::{Rect, Scale};
use tairix_theme::Theme;
use tairix_tz::listing::Listing;
use tairix_tz::testkit::utc;
use tairix_tz::ZoneInfo;

use crate::damage;
use crate::place::PlaceRow;
use crate::zone::{dialog, refresh, rows, Minute, TITLE};

fn zones() -> &'static ZoneInfo<'static> {
    let bytes: &'static [u8] = Box::leak(tairix_tz::testkit::store().into_boxed_slice());
    Box::leak(Box::new(
        ZoneInfo::parse(bytes).expect("the fixture parses"),
    ))
}

fn listing() -> Listing<'static> {
    let info = zones();
    Listing::of(info, info.region("GB"))
}

fn at(hour: i64, minute: i64, second: i64) -> Time64 {
    let secs = utc(2026, 7, 1, hour, minute).saturating_add(second);
    Time64::from_secs(secs)
}

fn london(rows: &[PlaceRow]) -> usize {
    rows.iter()
        .position(|row| row.key == "Europe/London")
        .expect("the fixture lists London")
}

#[test]
fn a_row_names_its_place_keys_its_zone_and_reads_only_with_a_time() {
    let listing = listing();
    let unread: Vec<PlaceRow> = rows(&listing, None).collect();
    assert_eq!(unread.len(), listing.entries().len());
    let row = &unread[london(&unread)];
    assert_eq!(row.name, "London");
    assert_eq!(row.region, "United Kingdom");
    assert!(row.reading.is_empty(), "no time held, so no reading");
    let read: Vec<PlaceRow> = rows(&listing, Some(at(13, 2, 0))).collect();
    assert_eq!(read[london(&read)].reading, "14:02 \u{b7} UTC+01:00");
}

#[test]
fn the_dialog_carries_the_title_and_highlights_the_place_held() {
    let listing = listing();
    let all: Vec<PlaceRow> = rows(&listing, None).collect();
    let held = london(&all);
    let shown = dialog(all, Some(held));
    assert_eq!(shown.title(), TITLE);
    assert_eq!(shown.chooser().highlighted(), Some(held));
    assert_eq!(dialog(Vec::new(), None).chooser().highlighted(), None);
}

#[test]
fn a_turned_minute_rewrites_the_readings_past_the_owners_rows() {
    let theme = Theme::dark();
    let listing = listing();
    let lead = PlaceRow {
        name: String::from("This machine's zone"),
        region: String::from("London, United Kingdom"),
        reading: String::from("kept by its owner"),
        key: String::from("default"),
    };
    let opened = at(13, 2, 10);
    let all: Vec<PlaceRow> = core::iter::once(lead)
        .chain(rows(&listing, Some(opened)))
        .collect();
    let row = london(&all);
    let mut shown = dialog(all, None);
    let (width, height) = shown.opening_extent(Scale::ONE, &theme);
    let frame = (Rect::new(0, 0, width, height), Scale::ONE, &theme);
    let mut written = Minute::of(Some(opened));

    let mut still = damage::sink();
    let turned = refresh(
        &mut shown,
        &mut written,
        (&listing, 1),
        at(13, 2, 59),
        frame,
        &mut still,
    );
    assert!(!turned, "the same minute rewrites nothing");
    assert!(still.is_empty());

    let mut moved = damage::sink();
    let turned = refresh(
        &mut shown,
        &mut written,
        (&listing, 1),
        at(13, 3, 0),
        frame,
        &mut moved,
    );
    assert!(turned);
    let rows = shown.chooser().rows();
    assert_eq!(rows[row].reading, "14:03 \u{b7} UTC+01:00");
    assert_eq!(rows[0].reading, "kept by its owner", "the owner's own row");
    assert!(!moved.is_empty(), "the rows that show are repainted");
}
