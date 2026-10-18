//! Host tests of the store reader: a valid store reads back what was built,
//! and every way a store can be malformed is refused whole.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Time64;

use crate::layout::{HEADER_LEN, NAME_LEN, TRANSITION_LEN, ZONE_LEN};
use crate::testkit::{reseal, store, store_input, utc, HOUR};
use crate::write::{build, BuildError};
use crate::{is_region_code, is_zone_name, StoreError, ZoneInfo};

#[test]
fn stores_are_equal_by_the_bytes_they_hold() {
    let bytes = store();
    let copy = bytes.clone();
    let one = ZoneInfo::parse(&bytes).expect("the fixture parses");
    let other = ZoneInfo::parse(&copy).expect("the copy parses");
    assert_eq!(one, other, "the same bytes in two places");
    let mut changed = store();
    let release = changed
        .windows(5)
        .position(|window| window == b"2026e")
        .expect("the release is stored");
    changed[release + 4] = b'f';
    reseal(&mut changed);
    let different = ZoneInfo::parse(&changed).expect("another release parses");
    assert_ne!(one, different);
}

#[test]
fn a_built_store_reads_back_what_it_was_built_from() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("the fixture parses");
    assert_eq!(info.release(), "2026e");
    let names: Vec<&str> = info.zones().map(|zone| zone.name()).collect();
    assert_eq!(
        names,
        [
            "America/New_York",
            "Asia/Kolkata",
            "Europe/Dublin",
            "Europe/London"
        ],
        "canonical zones in name order"
    );
    assert_eq!(info.default_zone().name(), "Europe/London");
    assert_eq!(info.default_region().map(|region| region.code), Some("GB"));
    let codes: Vec<&str> = info.regions().map(|region| region.code).collect();
    assert_eq!(codes, ["GB", "IE", "IN", "US"]);
    let us = info.region("US").expect("the United States");
    assert_eq!(
        (us.name, us.date.as_str(), us.time.as_str()),
        ("United States", "M/d/y", "h:mm a")
    );
    assert!(info.region("ZZ").is_none());
    let places: Vec<(&str, &str, &str)> = info
        .places()
        .map(|place| (place.zone.name(), place.region, place.comment))
        .collect();
    assert_eq!(
        places,
        [
            ("Europe/London", "GB", ""),
            ("America/New_York", "US", "Eastern (most areas)"),
            ("Asia/Kolkata", "IN", ""),
            ("Europe/Dublin", "IE", ""),
            ("Europe/Belfast", "GB", "Northern Ireland"),
        ],
        "in the table's own order, each by the name it lists"
    );
}

#[test]
fn an_alias_keeps_its_name_and_reads_its_canonical_zones_rules() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("the fixture parses");
    let alias = info.zone("Europe/Belfast").expect("the alias resolves");
    let london = info.zone("Europe/London").expect("London");
    assert_eq!(alias.name(), "Europe/Belfast");
    for at in [
        utc(1800, 1, 1, 0, 0),
        utc(2026, 7, 1, 0, 0),
        utc(2040, 1, 1, 0, 0),
    ] {
        let at = Time64::from_secs(at);
        assert_eq!(alias.offset_at(at), london.offset_at(at));
    }
    assert!(info.zone("Europe/Atlantis").is_none());
    assert!(info.zone("").is_none());
}

#[test]
fn a_places_city_is_its_names_last_part_read_with_spaces() {
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("the fixture parses");
    let cities: Vec<String> = info
        .places()
        .map(|place| {
            let mut city = String::new();
            place
                .write_city(&mut city)
                .expect("a String takes any write");
            city
        })
        .collect();
    assert_eq!(
        cities,
        ["London", "New York", "Kolkata", "Dublin", "Belfast"]
    );
}

/// `bytes` altered by `edit` with its checksum recomputed, and what parsing
/// it answers.
fn parsed_after(edit: impl FnOnce(&mut Vec<u8>)) -> Result<(), StoreError> {
    let mut bytes = store();
    edit(&mut bytes);
    reseal(&mut bytes);
    ZoneInfo::parse(&bytes).map(|_| ())
}

#[test]
fn a_store_whose_frame_is_wrong_is_refused() {
    let bytes = store();
    assert_eq!(
        ZoneInfo::parse(&bytes[..bytes.len() - 1]).map(|_| ()),
        Err(StoreError::Length)
    );
    assert_eq!(
        ZoneInfo::parse(&bytes[..HEADER_LEN - 1]).map(|_| ()),
        Err(StoreError::Header)
    );
    let mut longer = bytes.clone();
    longer.push(0);
    assert_eq!(
        ZoneInfo::parse(&longer).map(|_| ()),
        Err(StoreError::Length)
    );
    let mut flipped = bytes.clone();
    let last = flipped.len() - 2;
    flipped[last] ^= 0x20;
    assert_eq!(
        ZoneInfo::parse(&flipped).map(|_| ()),
        Err(StoreError::Checksum)
    );
    assert_eq!(
        parsed_after(|b| b[0] = b'X'),
        Err(StoreError::Header),
        "magic"
    );
    assert_eq!(
        parsed_after(|b| b[4] = 2),
        Err(StoreError::Header),
        "version"
    );
    assert_eq!(parsed_after(|b| b[6] = 1), Err(StoreError::Header), "flags");
    assert_eq!(ZoneInfo::parse(&[]).map(|_| ()), Err(StoreError::Header));
}

/// The checksum covers the header too: a default region or a release offset
/// changed in place is refused rather than read.
#[test]
fn a_store_whose_header_changed_under_its_checksum_is_refused() {
    let bytes = store();
    let mut region = bytes.clone();
    region[53] = b'R';
    assert_eq!(
        ZoneInfo::parse(&region).map(|_| ()),
        Err(StoreError::Checksum),
        "GB read as GR"
    );
    let mut release = bytes.clone();
    release[8] ^= 1;
    assert_eq!(
        ZoneInfo::parse(&release).map(|_| ()),
        Err(StoreError::Checksum)
    );
    let mut default = bytes;
    default[48] ^= 1;
    assert_eq!(
        ZoneInfo::parse(&default).map(|_| ()),
        Err(StoreError::Checksum)
    );
}

/// A refusal reads as a reason, and a store prints as its release and sizes
/// rather than every byte it holds.
#[test]
fn a_refusal_reads_as_a_reason_and_a_store_prints_small() {
    assert_eq!(
        alloc::format!("{}", StoreError::Checksum),
        "its checksum does not match its contents"
    );
    let bytes = store();
    let info = ZoneInfo::parse(&bytes).expect("parses");
    let printed = alloc::format!("{info:?}");
    assert!(printed.contains("2026e"), "{printed}");
    assert!(printed.len() < 160, "{printed}");
}

#[test]
fn a_store_whose_tables_are_out_of_order_is_refused() {
    let names = HEADER_LEN;
    assert_eq!(
        parsed_after(|b| {
            let (first, second) = b[names..names + 2 * NAME_LEN].split_at_mut(NAME_LEN);
            first.swap_with_slice(second);
        }),
        Err(StoreError::Order),
        "names out of order"
    );
    let info_bytes = store();
    let header = crate::layout::Header::decode(&info_bytes).expect("a header");
    let sections = crate::layout::Sections::of(&header).expect("sections");
    let london_first = |b: &mut Vec<u8>| {
        // Europe/London is the fourth zone in name order; its transitions are
        // the last four.
        let zone = sections.zones + 3 * ZONE_LEN;
        u32::from_le_bytes(b[zone + 4..zone + 8].try_into().expect("four bytes"))
    };
    assert_eq!(
        parsed_after(|b| {
            let first = usize::try_from(london_first(b)).expect("small");
            let at = sections.transitions + (first + 2) * TRANSITION_LEN;
            b[at..at + 8].copy_from_slice(&i64::MIN.to_le_bytes());
        }),
        Err(StoreError::Order),
        "a transition before the one before it"
    );
    assert_eq!(
        parsed_after(|b| {
            let at = sections.transitions;
            b[at..at + 8].copy_from_slice(&0i64.to_le_bytes());
        }),
        Err(StoreError::Order),
        "a zone whose first transition is not the start of time"
    );
    assert_eq!(
        parsed_after(|b| {
            // Swap the first two zones' names, and the zones the name records
            // point at, so every name still finds its zone and every zone
            // keeps its transitions, but the zones no longer stand in their
            // names' order.
            let zone = |index: usize| sections.zones + index * ZONE_LEN;
            let first_name: [u8; 4] = b[zone(0)..zone(0) + 4].try_into().expect("four bytes");
            b.copy_within(zone(1)..zone(1) + 4, zone(0));
            b[zone(1)..zone(1) + 4].copy_from_slice(&first_name);
            for record in b[sections.names..sections.zones]
                .as_chunks_mut::<NAME_LEN>()
                .0
            {
                let zone_index = u32::from_le_bytes(record[4..8].try_into().expect("four bytes"));
                let swapped = match zone_index {
                    0 => 1,
                    1 => 0,
                    other => other,
                };
                record[4..8].copy_from_slice(&swapped.to_le_bytes());
            }
        }),
        Err(StoreError::Order),
        "zones out of their names' order"
    );
}

#[test]
fn a_store_holding_a_value_outside_its_domain_is_refused() {
    let bytes = store();
    let header = crate::layout::Header::decode(&bytes).expect("a header");
    let sections = crate::layout::Sections::of(&header).expect("sections");
    assert_eq!(
        parsed_after(|b| {
            let at = sections.types + 4;
            b[at..at + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        }),
        Err(StoreError::Value),
        "an abbreviation index past the table"
    );
    assert_eq!(
        parsed_after(|b| {
            let at = sections.types;
            b[at..at + 4].copy_from_slice(&(30 * 3600i32).to_le_bytes());
        }),
        Err(StoreError::Value),
        "an offset past a day and two hours"
    );
    assert_eq!(
        parsed_after(|b| b[sections.types + 6] = 2),
        Err(StoreError::Value),
        "a daylight flag that is neither"
    );
    assert_eq!(
        parsed_after(|b| {
            let at = sections.transitions + 8;
            b[at..at + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        }),
        Err(StoreError::Index),
        "a transition to a type the table lacks"
    );
    assert_eq!(
        parsed_after(|b| b[sections.regions] = b'g'),
        Err(StoreError::Value),
        "a region code in lower case"
    );
    assert_eq!(
        parsed_after(|b| {
            let at = sections.places;
            b[at..at + 4].copy_from_slice(&99u32.to_le_bytes());
        }),
        Err(StoreError::Index),
        "a place in a zone the store lacks"
    );
    assert_eq!(
        parsed_after(|b| {
            let mut header = crate::layout::Header::decode(b).expect("a header");
            header.default_name = 99;
            b[..HEADER_LEN].copy_from_slice(&header.encode());
        }),
        Err(StoreError::Value),
        "a default zone the store lacks"
    );
    assert_eq!(
        parsed_after(|b| {
            let at = sections.trailing + 4 + 6;
            b[at] = 13;
        }),
        Err(StoreError::Value),
        "a trailing rule in a thirteenth month"
    );
}

/// Each phase of a trailing rule keeps its whole offset within a day and two
/// hours, as a type's is: a local time is searched no further either side.
#[test]
fn a_trailing_phase_whose_whole_offset_runs_past_the_bound_is_refused() {
    let mut input = store_input();
    let rule = input.zones[0]
        .trailing
        .as_mut()
        .expect("London keeps a trailing rule");
    rule.stdoff = 26 * HOUR;
    rule.daylight.save = 26 * HOUR;
    assert_eq!(
        build(&input),
        Err(BuildError::Refused(StoreError::Value)),
        "a standard offset and a saving each in bounds, summing past them"
    );
}

#[test]
fn the_writer_refuses_a_string_holding_its_terminator() {
    let mut input = store_input();
    input.places[1].comment = String::from("A\0B");
    assert_eq!(build(&input), Err(BuildError::Nul(String::from("A\0B"))));
}

#[test]
fn a_store_whose_strings_are_malformed_is_refused() {
    let bytes = store();
    let header = crate::layout::Header::decode(&bytes).expect("a header");
    let sections = crate::layout::Sections::of(&header).expect("sections");
    assert_eq!(
        parsed_after(|b| {
            let last = b.len() - 1;
            b[last] = b'x';
        }),
        Err(StoreError::String),
        "the last string unterminated"
    );
    let text = &bytes[sections.strings..];
    let date = text
        .windows(7)
        .position(|window| window == b"dd/MM/y")
        .expect("a date pattern");
    assert_eq!(
        parsed_after(|b| b[sections.strings + date] = b'q'),
        Err(StoreError::String),
        "a pattern no region may hold"
    );
}

#[test]
fn the_writer_refuses_input_no_store_can_hold() {
    let mut input = store_input();
    input
        .aliases
        .push((String::from("Europe/London"), String::from("Asia/Kolkata")));
    assert_eq!(
        build(&input),
        Err(BuildError::Duplicate(String::from("Europe/London")))
    );
    let mut input = store_input();
    input.aliases.push((
        String::from("Europe/Jersey"),
        String::from("Europe/Atlantis"),
    ));
    assert_eq!(
        build(&input),
        Err(BuildError::Unknown(String::from("Europe/Atlantis")))
    );
    let mut input = store_input();
    input.places[0].zone = String::from("Europe/Atlantis");
    assert_eq!(
        build(&input),
        Err(BuildError::Unknown(String::from("Europe/Atlantis")))
    );
    let mut input = store_input();
    input.regions[0].date = String::from("y");
    assert_eq!(build(&input), Err(BuildError::Refused(StoreError::String)));
    let mut input = store_input();
    input.regions[0].name = String::from("United \u{202e}setatS");
    assert_eq!(
        build(&input),
        Err(BuildError::Refused(StoreError::String)),
        "a name that reorders its own text"
    );
    let mut input = store_input();
    input.places[1].comment = String::from("Eastern\u{200b}");
    assert_eq!(
        build(&input),
        Err(BuildError::Refused(StoreError::String)),
        "a comment hiding a character"
    );
}

#[test]
fn a_zone_names_and_region_codes_shape_is_checked_before_any_lookup() {
    for name in [
        "Europe/London",
        "Etc/GMT+5",
        "America/Port-au-Prince",
        "UTC",
    ] {
        assert!(is_zone_name(name), "{name}");
    }
    for name in [
        "",
        "/Europe",
        "Europe/",
        "Europe//London",
        "../etc",
        "Europe/Lon don",
    ] {
        assert!(!is_zone_name(name), "{name:?}");
    }
    assert!(!is_zone_name(&"A".repeat(65)));
    assert!(is_region_code("GB"));
    for code in ["gb", "G", "GBR", "G1", ""] {
        assert!(!is_region_code(code), "{code:?}");
    }
}
