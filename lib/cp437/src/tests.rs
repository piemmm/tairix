//! Both readings against Unicode's mapping and the character generator's
//! glyphs, and each reverse lookup against the forward table.

use super::{byte_of, char_of, glyph_byte_of, glyph_of, REVERSE};

#[test]
fn text_is_ascii_below_the_upper_half_and_every_byte_is_spelt_back() {
    for byte in 0..=u8::MAX {
        let ch = char_of(byte);
        if byte < 0x80 {
            assert_eq!(u32::from(ch), u32::from(byte));
        }
        assert_eq!(byte_of(ch), Some(byte), "{byte:#04x}");
    }
}

#[test]
fn every_byte_but_nul_draws_a_visible_character_that_names_it_again() {
    for byte in 1..=u8::MAX {
        let ch = glyph_of(byte);
        assert!(!ch.is_control(), "{byte:#04x}");
        assert_eq!(glyph_byte_of(ch), Some(byte), "{byte:#04x}");
    }
    assert_eq!(glyph_of(0), ' ');
    assert_eq!(glyph_byte_of(' '), Some(0x20));
}

#[test]
fn no_two_bytes_draw_the_same_character() {
    assert!(REVERSE.windows(2).all(|pair| pair[0].0 < pair[1].0));
}

#[test]
fn the_upper_half_is_unicodes_mapping_read_and_drawn() {
    // One value in each sixteen of Unicode's MICSFT/PC/CP437.TXT, and its odd
    // entries: the peseta, the sharp s for beta, the micro sign for mu.
    let mapped = [
        (0x80, 0x00C7),
        (0x9E, 0x20A7),
        (0xA9, 0x2310),
        (0xB0, 0x2591),
        (0xC4, 0x2500),
        (0xDB, 0x2588),
        (0xE1, 0x00DF),
        (0xE6, 0x00B5),
        (0xF9, 0x2219),
        (0xFA, 0x00B7),
        (0xFC, 0x207F),
        (0xFF, 0x00A0),
    ];
    for (byte, scalar) in mapped {
        assert_eq!(u32::from(char_of(byte)), scalar, "{byte:#04x}");
        assert_eq!(glyph_of(byte), char_of(byte), "{byte:#04x}");
    }
}

#[test]
fn the_low_glyphs_are_the_character_generators() {
    let drawn = [
        (0x01, 0x263A),
        (0x03, 0x2665),
        (0x07, 0x2022),
        (0x0D, 0x266A),
        (0x10, 0x25BA),
        (0x14, 0x00B6),
        (0x15, 0x00A7),
        (0x1A, 0x2192),
        (0x1F, 0x25BC),
        (0x7F, 0x2302),
    ];
    for (byte, scalar) in drawn {
        assert_eq!(u32::from(glyph_of(byte)), scalar, "{byte:#04x}");
    }
}

#[test]
fn a_character_the_code_page_lacks_has_no_byte_either_way() {
    for ch in ['\u{20AC}', '\u{2014}', '\u{0101}', '\u{1F600}', '\u{FFFD}'] {
        assert_eq!(byte_of(ch), None, "{ch:?}");
        assert_eq!(glyph_byte_of(ch), None, "{ch:?}");
    }
}

#[test]
fn a_control_is_text_but_draws_nothing() {
    for ch in ['\0', '\t', '\n', '\r', '\u{1B}', '\u{7F}'] {
        assert_eq!(byte_of(ch), u8::try_from(ch).ok(), "{ch:?}");
        assert_eq!(glyph_byte_of(ch), None, "{ch:?}");
    }
    assert_eq!(byte_of('\u{263A}'), None, "a face is drawn, not spelt");
}
