use super::*;
use alloc::vec;
use alloc::vec::Vec;

/// Each line piece's arms, up, down, left and right: 0 none, 1 single, 2
/// double.
const ARMS: [(u8, [u8; 4]); 40] = [
    (0xB3, [1, 1, 0, 0]),
    (0xB4, [1, 1, 1, 0]),
    (0xB5, [1, 1, 2, 0]),
    (0xB6, [2, 2, 1, 0]),
    (0xB7, [0, 2, 1, 0]),
    (0xB8, [0, 1, 2, 0]),
    (0xB9, [2, 2, 2, 0]),
    (0xBA, [2, 2, 0, 0]),
    (0xBB, [0, 2, 2, 0]),
    (0xBC, [2, 0, 2, 0]),
    (0xBD, [2, 0, 1, 0]),
    (0xBE, [1, 0, 2, 0]),
    (0xBF, [0, 1, 1, 0]),
    (0xC0, [1, 0, 0, 1]),
    (0xC1, [1, 0, 1, 1]),
    (0xC2, [0, 1, 1, 1]),
    (0xC3, [1, 1, 0, 1]),
    (0xC4, [0, 0, 1, 1]),
    (0xC5, [1, 1, 1, 1]),
    (0xC6, [1, 1, 0, 2]),
    (0xC7, [2, 2, 0, 1]),
    (0xC8, [2, 0, 0, 2]),
    (0xC9, [0, 2, 0, 2]),
    (0xCA, [2, 0, 2, 2]),
    (0xCB, [0, 2, 2, 2]),
    (0xCC, [2, 2, 0, 2]),
    (0xCD, [0, 0, 2, 2]),
    (0xCE, [2, 2, 2, 2]),
    (0xCF, [1, 0, 2, 2]),
    (0xD0, [2, 0, 1, 1]),
    (0xD1, [0, 1, 2, 2]),
    (0xD2, [0, 2, 1, 1]),
    (0xD3, [2, 0, 0, 1]),
    (0xD4, [1, 0, 0, 2]),
    (0xD5, [0, 1, 0, 2]),
    (0xD6, [0, 2, 0, 1]),
    (0xD7, [2, 2, 1, 1]),
    (0xD8, [1, 1, 2, 2]),
    (0xD9, [1, 0, 1, 0]),
    (0xDA, [0, 1, 0, 1]),
];

/// The columns a vertical arm crosses the cell's edge on, as a row mask.
fn vertical(style: u8) -> u8 {
    [0x00, 0x10, 0x28][usize::from(style)]
}

/// The rows a horizontal arm crosses the cell's edge on.
fn horizontal(style: u8, centre: usize) -> Vec<usize> {
    match style {
        0 => vec![],
        1 => vec![centre],
        _ => vec![centre - 1, centre + 1],
    }
}

fn check_lines<const H: usize>(font: &Font<H>, centre: usize) {
    for (code, [up, down, left, right]) in ARMS {
        let glyph = font.glyph(code);
        assert_eq!(glyph[0], vertical(up), "{code:02X} top edge, {H} rows");
        assert_eq!(
            glyph[H - 1],
            vertical(down),
            "{code:02X} bottom edge, {H} rows"
        );
        let column = |bit: u8| (0..H).filter(|&r| glyph[r] & bit != 0).collect::<Vec<_>>();
        assert_eq!(
            column(0x80),
            horizontal(left, centre),
            "{code:02X} left edge, {H} rows"
        );
        assert_eq!(
            column(0x01),
            horizontal(right, centre),
            "{code:02X} right edge, {H} rows"
        );
    }
}

#[test]
fn every_line_piece_meets_its_neighbours_in_every_size() {
    check_lines(&CGA, 3);
    check_lines(&EGA, 6);
    check_lines(&VGA, 7);
}

#[test]
fn every_line_piece_with_a_right_arm_is_one_the_ninth_column_joins() {
    for (code, arms) in ARMS {
        assert!(arms[3] == 0 || joins_ninth_column(code), "{code:02X}");
    }
}

#[test]
fn the_tall_set_pads_text_and_carries_lines() {
    for code in 0..=255u8 {
        let carried = joins_vertically(usize::from(code)) || (0xB0..=0xB2).contains(&code);
        if !carried {
            assert_eq!((VGA.row(code, 0), VGA.row(code, 15)), (0, 0), "{code:02X}");
            assert_eq!(&VGA.glyph(code)[1..15], EGA.glyph(code), "{code:02X}");
        }
    }
    assert_eq!(VGA.glyph(0xDB), &[0xFF; 16]);
    assert_eq!(
        VGA.glyph(0xDC)[7..],
        [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
    );
    for row in 0..16 {
        assert_eq!(
            VGA.row(0xB1, row),
            if row % 2 == 0 { 0xAA } else { 0x55 },
            "the medium shade checkers on"
        );
    }
}

#[test]
fn printable_characters_are_all_different() {
    fn distinct<const H: usize>(font: &Font<H>) {
        for a in 0x21..=0x7Eu8 {
            for b in a + 1..=0x7E {
                assert_ne!(
                    font.glyph(a),
                    font.glyph(b),
                    "{a:02X} and {b:02X} in {H} rows"
                );
            }
        }
    }
    distinct(&CGA);
    distinct(&EGA);
    distinct(&VGA);
}

#[test]
fn space_and_no_break_space_are_dark_and_the_full_block_is_lit() {
    for code in [0x00, 0x20, 0xFF] {
        assert!(CGA
            .glyph(code)
            .iter()
            .chain(EGA.glyph(code))
            .all(|&r| r == 0));
    }
    assert!(CGA.glyph(0xDB).iter().all(|&r| r == 0xFF));
    assert_eq!(EGA.row(0x41, 3), 0b0011_1000, "'A' starts on the cap line");
    assert_eq!(VGA.row(0x41, 4), 0b0011_1000);
    assert_eq!(CGA.row(0x41, 0), 0b0011_1000);
}
