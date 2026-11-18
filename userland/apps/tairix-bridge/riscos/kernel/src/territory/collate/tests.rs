//! The UK territory's text tests: its tables, its collation and the
//! transformation that sorts as collation does.

use super::*;
use tairix_fsmeta::preset::acorn::UPPER_CASE;

/// Whether `c` has property `n`.
fn has(n: usize, c: u8) -> bool {
    PROPERTY_MAPS[n][usize::from(c >> 3)] >> (c & 7) & 1 != 0
}

#[test]
fn the_tables_pair_cases_and_read_digits() {
    for c in b'A'..=b'Z' {
        assert_eq!(LOWER_CASE[usize::from(c)], c + 0x20);
        assert_eq!(UPPER_CASE[usize::from(c + 0x20)], c);
    }
    assert_eq!(LOWER_CASE[0xC9], 0xE9, "É");
    assert_eq!(UPPER_CASE[0xFF], b'Y', "ÿ has no capital");
    assert_eq!(LOWER_CASE[0xD7], 0xD7, "× is no letter");
    assert_eq!(PLAIN[0xE9], b'e');
    assert_eq!(PLAIN[0xC6], 0xC6, "Æ keeps its shape");
    assert_eq!(CONTROL[usize::from(b'G')], 0x07);
    assert_eq!(CONTROL[usize::from(b'2')], 0x02);
    assert_eq!(CONTROL[0xA3], 0x03, "£");
    assert_eq!(
        (
            VALUE[usize::from(b'f')],
            VALUE[usize::from(b'9')],
            VALUE[usize::from(b'g')]
        ),
        (15, 9, 0)
    );
}

#[test]
fn properties_hold_what_latin1_says() {
    assert!(has(1, b'Q') && !has(2, b'Q'));
    assert!(has(2, 0xDF) && !has(1, 0xDF), "ß is lower case");
    assert!(has(3, 0x9E), "the ligatures are letters");
    assert!(has(5, b' ') && has(5, 0xA0) && !has(5, b'x'));
    assert!(has(6, b'7') && !has(6, b'a'));
    assert!(has(7, b'c') && !has(7, b'g'));
    assert!(has(8, 0xE9) && !has(8, b'e'));
    assert!(has(9, 0x00) && has(9, 0xFF));
    assert!((0..=255).all(|c| !has(10, c)));
}

#[test]
fn collation_orders_by_letter_then_accent_then_case() {
    assert!(collate(b"apple", b"Banana", 0) < 0);
    assert!(
        collate(b"A", b"a", 0) < 0,
        "capitals first where all else is equal"
    );
    assert_eq!(collate(b"A", b"a", IGNORE_CASE), 0);
    assert!(
        collate(b"e", b"\xE9", 0) < 0 && collate(b"\xE9", b"f", 0) < 0,
        "é between e and f"
    );
    assert_eq!(
        collate(b"e", b"\xE9", IGNORE_ACCENTS),
        0,
        "case weighed on the unaccented letters"
    );
    assert!(
        collate(b"E", b"\xE9", IGNORE_ACCENTS) < 0,
        "and still told apart by it"
    );
    assert_eq!(collate(b"e", b"\xE9", IGNORE_ACCENTS | IGNORE_CASE), 0);
    assert!(collate(b"ab", b"abc", 0) < 0, "the shorter first");
    assert_eq!(collate(b"\x9Ex", b"fix", 0), 0, "ﬁ is f and i");
}

#[test]
fn numeric_collation_reads_digit_runs_as_numbers() {
    assert!(collate(b"file10", b"file9", 0) < 0);
    assert!(collate(b"file10", b"file9", NUMERIC) > 0);
    assert_eq!(collate(b"file007", b"file7", NUMERIC), 0);
    assert!(collate(b"file12", b"file13", NUMERIC) < 0);
}

#[test]
fn transformed_strings_sort_as_they_collate() {
    let words: [&[u8]; 8] = [
        b"apple",
        b"Apple",
        b"\xE9t\xE9",
        b"ete",
        b"banana",
        b"\x9Eg",
        b"fig",
        b"\x01x",
    ];
    let transformed = |w: &[u8]| {
        let mut out = alloc::vec::Vec::new();
        transform(w, &mut out);
        out
    };
    for a in words {
        for b in words {
            let collated = collate(a, b, 0).signum();
            let sorted = match transformed(a).cmp(&transformed(b)) {
                core::cmp::Ordering::Less => -1,
                core::cmp::Ordering::Equal => 0,
                core::cmp::Ordering::Greater => 1,
            };
            assert_eq!(collated, sorted, "{a:?} against {b:?}");
        }
    }
    assert_eq!(
        transformed(b"\x01"),
        [2, 1, 1, 2, 1, 1, 2, 1, 1],
        "a one escaped in each pass"
    );
}
