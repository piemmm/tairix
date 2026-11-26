//! What the listings make of a file's bytes.

use alloc::vec::Vec;

use super::{Listing, Progress, ANGLED, HEXADECIMAL, RAW_TOP, SEVEN_BIT, TAB_SPACES};

/// `bytes` listed as `listing` lists them, the file's end included.
fn listed(listing: Listing, bytes: &[u8]) -> Vec<u8> {
    let mut progress = Progress::default();
    let mut out = Vec::new();
    for &byte in bytes {
        listing.feed(&mut progress, byte, &mut out);
    }
    listing.end(progress, &mut out);
    out
}

fn filtered(format: u8, numbered: bool) -> Listing {
    Listing {
        numbered,
        ..Listing::filtered(format)
    }
}

#[test]
fn a_listing_ends_lines_at_cr_lf_or_a_pair_of_them_and_numbers_each() {
    assert_eq!(
        listed(filtered(0, true), b"one\r\ntwo\n\nthree"),
        b"   1 one\n\r   2 two\n\r   3 \n\r   4 three\n\r",
    );
    assert_eq!(
        listed(filtered(0, false), b"one\n\rtwo\r"),
        b"one\n\rtwo\n\r",
        "LF CR is a pair too"
    );
    assert_eq!(
        listed(filtered(0, false), b""),
        b"",
        "nothing for an empty file"
    );
}

#[test]
fn line_numbers_take_four_columns_until_they_need_more() {
    let lines: Vec<u8> = (0..1000).flat_map(|_| *b"x\n").collect();
    let text = listed(filtered(0, true), &lines);
    assert!(text.starts_with(b"   1 x\n\r"));
    assert!(text.ends_with(b" 999 x\n\r1000 x\n\r"));
}

#[test]
fn an_unprintable_byte_is_shown_as_gstrans_would_read_it_back() {
    let bytes = b"\x07|\"<\x7F\xA2\xC1";
    assert_eq!(listed(filtered(0, false), bytes), b"|G||\"<|?|!|\"|!A\n\r");
    assert_eq!(
        listed(filtered(ANGLED, false), bytes),
        b"<7><124>\"<<127><162><193>\n\r"
    );
    assert_eq!(
        listed(filtered(ANGLED | HEXADECIMAL, false), bytes),
        b"<&07><&7C>\"<<&7F><&A2><&C1>\n\r",
    );
    assert_eq!(listed(filtered(HEXADECIMAL, false), bytes), b".|\"<...\n\r");
    assert_eq!(
        listed(filtered(RAW_TOP, false), bytes),
        b"|G||\"<|?\xA2\xC1\n\r"
    );
    assert_eq!(
        listed(filtered(SEVEN_BIT, false), bytes),
        b"|G||\"<|?|\"A\n\r"
    );
}

#[test]
fn a_tab_is_eight_spaces_only_when_asked_for() {
    let expanding = Listing {
        tabs: true,
        ..filtered(0, false)
    };
    assert_eq!(listed(expanding, b"a\tb"), b"a        b\n\r");
    assert_eq!(listed(filtered(0, false), b"a\tb"), b"a|Ib\n\r");
}

#[test]
fn a_filter_reads_only_the_dump_format_bits_it_shows_bytes_by() {
    assert_eq!(
        listed(Listing::filtered(TAB_SPACES | RAW_TOP | 0xE0), b"\t\x80"),
        b"|I\x80\n\r",
        "vdu: leaves its tabs unexpanded whatever DumpFormat says"
    );
}

#[test]
fn print_writes_each_byte_as_it_is_and_nothing_more() {
    assert_eq!(listed(Listing::RAW, b"a\r\n\x07"), b"a\r\n\x07");
}
