use std::string::ToString;

use crate::{parse_hex, HexForm, Rgb, Rgba};

#[test]
fn every_css_form_is_read_in_either_case() {
    assert_eq!(
        parse_hex("1a2B3c"),
        Some((Rgba::rgb(0x1a, 0x2b, 0x3c), HexForm::Long))
    );
    assert_eq!(
        parse_hex("1A2b3C80"),
        Some((Rgba::new(0x1a, 0x2b, 0x3c, 0x80), HexForm::LongAlpha))
    );
    assert_eq!(
        parse_hex("fA0"),
        Some((Rgba::rgb(0xff, 0xaa, 0x00), HexForm::Short))
    );
    assert_eq!(
        parse_hex("fa08"),
        Some((Rgba::new(0xff, 0xaa, 0x00, 0x88), HexForm::ShortAlpha))
    );
    assert!(HexForm::LongAlpha.has_alpha() && HexForm::ShortAlpha.has_alpha());
    assert!(!HexForm::Long.has_alpha() && !HexForm::Short.has_alpha());
}

#[test]
fn anything_but_the_digits_is_refused() {
    for text in [
        "",
        "f",
        "ff",
        "fffff",
        "fffffff",
        "fffffffff",
        "#fff",
        "#ffffff",
        " ffffff",
        "ffffff ",
        "gggggg",
        "12345g",
        "-12345",
    ] {
        assert_eq!(parse_hex(text), None, "{text:?}");
    }
}

#[test]
fn a_sign_is_not_a_digit() {
    // An integer parser takes `+f` for fifteen; a colour's digits carry no
    // sign.
    for text in ["+f+f+f", "+fff", "+1+2+3+4"] {
        assert_eq!(parse_hex(text), None, "{text:?}");
    }
    assert_eq!(Rgb::from_hex("+f+f+f"), None);
}

#[test]
fn a_character_that_is_not_ascii_is_refused_whole() {
    assert_eq!(parse_hex("ééé"), None, "six bytes, no digits");
    assert_eq!(parse_hex("ff\u{00e9}f"), None);
}

#[test]
fn an_opaque_colour_is_six_digits_and_no_other_form() {
    assert_eq!(Rgb::from_hex("0e141B"), Some(Rgb::new(0x0e, 0x14, 0x1b)));
    for other in ["fff", "ffff", "ffffffff", "#ffffff"] {
        assert_eq!(Rgb::from_hex(other), None, "{other:?}");
    }
}

#[test]
fn colours_are_written_in_lowercase_digits() {
    assert_eq!(Rgb::new(0x0e, 0x14, 0xab).hex().to_string(), "0e14ab");
    assert_eq!(Rgba::rgb(0xAB, 0xCD, 0xEF).hex().to_string(), "abcdef");
    assert_eq!(Rgba::new(1, 2, 3, 0x80).hex().to_string(), "01020380");
    assert_eq!(Rgba::new(1, 2, 3, 0).hex().to_string(), "01020300");
    assert_eq!(
        Rgba::new(1, 2, 3, 0x80).hex().hashed().to_string(),
        "#01020380"
    );
    assert_eq!(Rgb::WHITE.hex().hashed().to_string(), "#ffffff");
}

#[test]
fn what_is_written_reads_back() {
    for packed in (0..=0x00FF_FFFFu32).step_by(97) {
        let [alpha, r, g, b] = packed.wrapping_mul(2_654_435_761).to_be_bytes();
        let rgb = Rgb::new(r, g, b);
        assert_eq!(Rgb::from_hex(&rgb.hex().to_string()), Some(rgb));
        let rgba = rgb.with_alpha(alpha);
        let written = rgba.hex().to_string();
        let (read, form) = parse_hex(&written).expect("written digits read back");
        assert_eq!(read, rgba);
        assert_eq!(form.has_alpha(), !rgba.is_opaque());
    }
}
