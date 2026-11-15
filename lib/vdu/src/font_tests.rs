use super::*;

/// The glyph RISC OS's arrangement gives `code`.
fn risc_os(code: u8) -> Glyph {
    RISC_OS[usize::from(code - FIRST)]
}

#[test]
fn a_glyph_s_rows_are_its_drawing_s() {
    assert_eq!(
        risc_os(b'A'),
        [0x18, 0x3C, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x00]
    );
    assert_eq!(risc_os(127), [0xFF; 8], "RISC OS's solid block");
    assert_eq!(
        BBC[usize::from(127 - FIRST)],
        [0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55]
    );
}

#[test]
fn the_bbc_s_arrangement_is_risc_os_s_but_for_96_and_127() {
    for code in FIRST..=127 {
        let bbc = BBC[usize::from(code - FIRST)];
        match code {
            96 => assert_eq!(bbc, risc_os(163), "the pound sign"),
            127 => assert_ne!(bbc, risc_os(127)),
            _ => assert_eq!(bbc, risc_os(code), "{code}"),
        }
    }
    assert_ne!(
        risc_os(96),
        risc_os(163),
        "RISC OS's 96 is the grave accent"
    );
}

#[test]
fn no_two_characters_share_a_picture_but_those_drawn_alike_on_purpose() {
    let alike = [
        (32, 160),
        (34, 149),
        (39, 145),
        (45, 153),
        (45, 173),
        (153, 173),
    ];
    for first in FIRST..255 {
        for second in first + 1..=255 {
            let same = risc_os(first) == risc_os(second);
            assert_eq!(
                same,
                alike.contains(&(first, second)),
                "{first} and {second}"
            );
        }
    }
}

#[test]
fn an_accented_letter_keeps_its_letter_below_its_accent() {
    for (accented, letter) in [
        (b'\xE9', b'e'),
        (b'\xF1', b'n'),
        (b'\xFC', b'u'),
        (b'\xE0', b'a'),
    ] {
        assert_eq!(risc_os(accented)[2..], risc_os(letter)[2..], "{accented}");
        assert_eq!(risc_os(letter)[..2], [0, 0]);
        assert_ne!(risc_os(accented)[..2], [0, 0]);
    }
}
