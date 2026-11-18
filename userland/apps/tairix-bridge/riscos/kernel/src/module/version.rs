//! Module versions in binary-coded decimal, as RISC OS reads them: the last
//! four digits of the whole number in the top half, the first four of the
//! fraction in the bottom, so that comparing two words orders the versions.

use alloc::vec::Vec;

/// The column a help string's version starts at, or after.
const COLUMN: u32 = 16;

/// The version `text` starts with, past its spaces; zero for none.
pub(crate) fn parse(text: &[u8]) -> u32 {
    let text = &text[text.iter().take_while(|&&b| b == b' ').count()..];
    let whole_digits = text.iter().take_while(|b| b.is_ascii_digit()).count();
    let whole = text[..whole_digits]
        .iter()
        .fold(0u32, |n, &d| (n << 4) | u32::from(d - b'0'));
    let mut fraction = 0;
    if text.get(whole_digits) == Some(&b'.') {
        for (shift, &d) in [12, 8, 4, 0].into_iter().zip(
            text[whole_digits + 1..]
                .iter()
                .take_while(|b| b.is_ascii_digit()),
        ) {
            fraction |= u32::from(d - b'0') << shift;
        }
    }
    (whole << 16) | fraction
}

/// The version a module's help string gives: the first number from its
/// sixteenth column on, tabs stopping every eight; zero if a control
/// character other than a tab comes first.
pub(crate) fn of_help(help: &[u8]) -> u32 {
    let mut column = 0;
    let mut at = 0;
    while column < COLUMN {
        let Some(&c) = help.get(at) else {
            return 0;
        };
        if c == 0 {
            return 0;
        }
        column = if c == b'\t' {
            (column + 8) & !7
        } else {
            column + 1
        };
        at += 1;
    }
    for (i, &c) in help.iter().enumerate().skip(at) {
        if c != b'\t' && c < 31 {
            return 0;
        }
        if c.is_ascii_digit() {
            return parse(&help[i..]);
        }
    }
    0
}

/// `version` as `*ROMModules` shows it: the whole number without leading
/// zeros, then two places of the fraction, and a third and fourth only where
/// what is left of it is not zero.
pub(crate) fn text(version: u32) -> Vec<u8> {
    let nibble = |n: u32| (version >> (28 - 4 * n)).to_le_bytes()[0] & 0xF;
    let digit = |n: u32| b'0' + nibble(n);
    let mut out = Vec::new();
    let first = (0..3).find(|&n| nibble(n) != 0).unwrap_or(3);
    out.extend((first..4).map(digit));
    out.push(b'.');
    out.extend((4..6).map(digit));
    let mut rest = version << 24;
    for n in 6..8 {
        if rest == 0 {
            break;
        }
        out.push(digit(n));
        rest <<= 4;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_read_as_binary_coded_decimal() {
        assert_eq!(parse(b"2.34"), 0x0002_3400);
        assert_eq!(parse(b"5.6789"), 0x0005_6789);
        assert_eq!(parse(b"17"), 0x0017_0000);
        assert_eq!(parse(b"  1.2"), 0x0001_2000);
        assert_eq!(parse(b"12345.6"), 0x2345_6000);
        assert_eq!(parse(b"1.23456"), 0x0001_2345);
        assert_eq!(parse(b"1.2.3"), 0x0001_2000);
        assert_eq!(parse(b"abc"), 0);
    }

    #[test]
    fn comparing_the_words_orders_the_versions() {
        assert!(parse(b"1.2") > parse(b"1.10"));
        assert_eq!(parse(b"1.1"), parse(b"1.1000"));
        assert!(parse(b"1.02") < parse(b"1.1"));
        assert!(parse(b"10") > parse(b"9.99"));
        assert_eq!(parse(b"1.00001"), parse(b"1.0"));
    }

    #[test]
    fn a_help_string_gives_the_number_from_its_sixteenth_column() {
        assert_eq!(of_help(b"UtilityModule\t5.30 (24 Apr 2024)\0"), 0x0005_3000);
        assert_eq!(of_help(b"Desktop\t\t2.77 (26 Oct 2018)\0"), 0x0002_7700);
        assert_eq!(of_help(b"Title\t\t(01 Jan 2000)\0"), 0x0001_0000);
        assert_eq!(of_help(b"Title 1.00\0"), 0);
        // A fifteen-character title loses the version's first digit.
        assert_eq!(of_help(b"FifteenCharName1.00\0"), 0);
        assert_eq!(of_help(b"Title\t\t\x01 1.00\0"), 0);
        assert_eq!(of_help(b"Title\t\t\x1F1.00\0"), 0x0001_0000);
    }

    #[test]
    fn versions_print_as_rom_modules_shows_them() {
        assert_eq!(text(0x0005_3000), b"5.30");
        assert_eq!(text(0x0123_4500), b"123.45");
        assert_eq!(text(0x0001_2305), b"1.2305");
        assert_eq!(text(0x0001_2350), b"1.235");
        assert_eq!(text(0), b"0.00");
        assert_eq!(text(0x000A_0000), b":.00");
    }
}
