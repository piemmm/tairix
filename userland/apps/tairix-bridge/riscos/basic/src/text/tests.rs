extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use super::*;
use crate::five::{Five, EXPONENT_DIGITS};

fn read(text: &str) -> (bool, Vec<u8>, i32, usize) {
    let number = Written::val(text.as_bytes(), EXPONENT_DIGITS);
    (
        number.negative,
        number.digits().collect(),
        number.exponent,
        number.length,
    )
}

fn five(text: &str) -> Five {
    Five::read(&Written::val(text.as_bytes(), EXPONENT_DIGITS)).unwrap()
}

fn written(value: Five, at: u32) -> String {
    let mut out = String::new();
    write(&value, Format::of_str(at), &mut out).unwrap();
    out
}

#[test]
fn a_number_is_digits_a_point_and_an_exponent() {
    assert_eq!(read("  -12.5E3xyz"), (true, alloc::vec![1, 2, 5], 2, 9));
    assert_eq!(read("+.5"), (false, alloc::vec![5], -1, 3));
    assert_eq!(read("5."), (false, alloc::vec![5], 0, 2));
    assert_eq!(read("1.2.3"), (false, alloc::vec![1, 2], -1, 3));
    assert_eq!(
        Written::literal(b"0007", EXPONENT_DIGITS).map(|n| n.length),
        Some(4)
    );
}

#[test]
fn an_exponent_is_upper_case_and_two_digits_at_most() {
    assert_eq!(read("1E123").2, 12);
    assert_eq!(read("1E-0001").2, 0);
    assert_eq!(read("1E+5").2, 5);
    assert_eq!(read("1e5"), (false, alloc::vec![1], 0, 1));
}

#[test]
fn each_dialect_reads_its_own_integers_as_recorded() {
    let integer =
        |text: &str, integers| Written::val(text.as_bytes(), EXPONENT_DIGITS).integer(integers);
    for (text, five, six) in [
        ("2147483647", Some(2_147_483_647), None),
        ("2147483648", None, None),
        ("-7", Some(-7), Some(-7)),
        ("1E0", Some(1), None),
        ("1E-0", Some(1), None),
        ("1E", Some(1), None),
        ("1E1", None, None),
        ("0E5", None, None),
        ("5.", None, None),
        ("5.0", None, None),
        ("213909503", Some(213_909_503), Some(213_909_503)),
        ("213909504", Some(213_909_504), None),
        ("0213909504", Some(213_909_504), None),
        ("00000000001", Some(1), Some(1)),
    ] {
        assert_eq!(integer(text, Integers::Five), five, "BASIC V {text}");
        assert_eq!(integer(text, Integers::Six), six, "BASIC VI {text}");
    }
}

#[test]
fn text_with_no_number_reads_as_zero() {
    for text in ["", "-", "- 5", "+-5", "&FF", "%101", "E5", "."] {
        assert_eq!(read(text), (false, alloc::vec![], 0, 0), "{text:?}");
    }
}

#[test]
fn every_digit_of_a_long_number_counts() {
    // 2^32 + 1 lies half way between 2^32 and 2^32 + 2, and goes to the odd
    // mantissa above; a long enough run of nines stays short of it.
    assert_eq!(five("4294967297"), five("4294967298"));
    assert_eq!(
        five("4294967296.9999999999999999999999999"),
        five("4294967296")
    );
    assert_ne!(five("4294967296"), five("4294967298"));
}

#[test]
fn reading_past_the_range_is_too_big_and_below_it_zero() {
    assert_eq!(
        Five::read(&Written::val(b"1E39", EXPONENT_DIGITS)),
        Err(crate::Fault::TooBig)
    );
    assert_eq!(five("1E-40"), Five::ZERO);
}

#[test]
fn str_ignores_the_layout_unless_told_to_use_it() {
    let eighth = five("0.125");
    assert_eq!(written(eighth, 0x90A), "0.125");
    assert_eq!(written(eighth, 0x0100_0100), "0.1");
}

#[test]
fn the_layout_byte_s_top_bit_writes_a_decimal_comma() {
    let value = five("1.5");
    assert_eq!(written(value, 0x0180_0400), "1,5");
    assert_eq!(written(value, 0x0181_0400), "1,500E0  ");
    assert_eq!(written(value, 0x0182_0400), "1,5000");
    assert_eq!(
        written(value, 0x01C1_0400),
        "1,5",
        "another bit set is general"
    );
}

#[test]
fn the_general_layout_turns_to_exponents_outside_its_digits() {
    assert_eq!(written(five("0.0625"), 0x0100_0A00), "0.0625");
    assert_eq!(written(five("0.001953125"), 0x0100_0A00), "1.953125E-3");
    // A hundredth is held just below itself.
    assert_eq!(written(five("0.01"), 0x0100_0A00), "9.999999998E-3");
    assert_eq!(written(five("0.001"), 0x0100_0A00), "1E-3");
    assert_eq!(written(five("1234567890"), 0x0100_0900), "1.23456789E9");
    assert_eq!(written(five("999999999.5"), 0x0100_0900), "1E9");
    assert_eq!(written(Five::ZERO, 0x0100_0A00), "0");
}

#[test]
fn the_exponent_layout_keeps_its_digits_and_pads_the_exponent() {
    assert_eq!(written(five("1"), 0x0101_0100), "1.E0  ");
    assert_eq!(written(five("0.15"), 0x0101_0100), "2.E-1 ");
    assert_eq!(written(Five::ZERO, 0x0101_0500), "0.0000E0  ");
    assert_eq!(written(Five::ZERO, 0x0101_0000), "0.0000000000E0  ");
}

#[test]
fn the_fixed_layout_rounds_at_its_places_within_the_digits() {
    assert_eq!(written(five("0.5"), 0x0102_0000), "1.");
    assert_eq!(written(five("0.15"), 0x0102_0000), "0.");
    assert_eq!(written(five("1"), 0x0102_0200), "1.00");
    assert_eq!(written(five("0.5"), 0x0102_1400), "0.50000000000");
    assert_eq!(written(five("1.5"), 0x0102_1400), "1.5");
    assert_eq!(written(five("1E11"), 0x0102_0000), "1E11");
    assert_eq!(written(five("999999999.5"), 0x0102_0000), "1000000000.");
    assert_eq!(written(Five::ZERO, 0x0102_FF00), "0.00000000000");
    assert_eq!(written(five("-0.004"), 0x0102_0200), "-0.00");
}

#[test]
fn at_percent_given_a_string_is_read_as_genuine_basic_reads_it() {
    // Each from &01020304 as recorded, then four from a decimal comma.
    let cases: &[(&str, u32)] = &[
        ("G10.9", 0x90A),
        ("E10.3", 0x1_030A),
        ("F10.2", 0x2_020A),
        ("G10,9", 0x80_090A),
        ("+G10.9", 0x100_090A),
        ("g10.9", 0x90A),
        ("G", 0x304),
        ("G10", 0x30A),
        (".5", 0x2_0504),
        ("G.5", 0x504),
        ("X10.2", 0x2_0304),
        ("", 0x2_0304),
        ("G300.2", 0x22C),
        ("G10.30", 0x1E0A),
        ("F 10.2", 0x2_0304),
        ("E10.3X", 0x1_030A),
        ("+", 0x102_0304),
        ("10.2", 0x2_020A),
        ("5", 0x2_0305),
        (",", 0x82_0304),
        (".", 0x2_0304),
        ("F10.", 0x2_030A),
        ("E.", 0x1_0304),
        ("+F,2", 0x182_0204),
        (" G10.2", 0x2_0304),
        ("G10.2 ", 0x20A),
        ("G-1.2", 0x304),
        ("F10.2.3", 0x2_020A),
        ("G255.255", 0xFFFF),
        ("G256", 0x300),
        ("F0.0", 0x2_0000),
        ("E99", 0x1_0363),
        ("++G", 0x102_0304),
        ("G1E1", 0x301),
    ];
    for &(text, expected) in cases {
        assert_eq!(
            at_percent_of(0x0102_0304, text.as_bytes()),
            expected,
            "{text:?}"
        );
    }
    for (text, expected) in [
        ("G", 0x80_0000),
        ("F10", 0x82_000A),
        ("+", 0x180_0000),
        ("E5.", 0x1_0005),
    ] {
        assert_eq!(
            at_percent_of(0x80_0000, text.as_bytes()),
            expected,
            "{text:?}"
        );
    }
}

#[test]
fn at_percent_is_shown_as_the_string_that_sets_it() {
    for (at, expected) in [
        (0x90A, "G10.9"),
        (0x2_0105, "F5.1"),
        (0x1_0A0A, "E10.10"),
        (0x302_0304, "+F4.3"),
        (0xFF, "G255.0"),
        (0x180_0000, "+G0,0"),
        (0, "G0.0"),
    ] {
        let mut out = String::new();
        write_at_percent(at, &mut out).unwrap();
        assert_eq!(out, expected, "{at:#X}");
        assert_eq!(
            at_percent_of(0, out.as_bytes()) & 0x1FF_FFFF,
            at & 0x1FF_FFFF,
            "{out} reads back"
        );
    }
}
