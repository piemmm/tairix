//! The character ROM, drawn for this machine: two sets, each 128 glyphs and
//! then their reverses, upper case with the graphics and then lower case with
//! upper case, eight bytes a glyph with the leftmost pixel the top bit.
//!
//! The letters, digits and punctuation are `lib/vdu`'s in the BBC's
//! arrangement, as the clean-room MOS has them. The graphics
//! are drawn from their functions, as Unicode's legacy computing symbols name
//! the PETSCII set: lines and blocks at the eighths they are named for, box
//! drawing in strokes two pixels wide through the middle of the cell. The
//! pictures (the card suits, circles, π, the tick and the arrows) are this
//! ROM's own.

use alloc::vec::Vec;

use crate::machine::c64::memory::{CHARACTER_BYTES, LOWER_CASE_SET};
use crate::rom::asm::{AsmError, Image};
use tairix_vdu::font::{bbc_glyph, draw, Glyph};

/// Bytes a glyph takes.
const GLYPH: usize = size_of::<Glyph>();
/// Glyphs in a set, its reverses aside.
const SET_GLYPHS: usize = 128;
/// Bytes of a set and its reverses.
const SET_BYTES: usize = 2 * SET_GLYPHS * GLYPH;
const _: () = assert!(
    SET_BYTES == LOWER_CASE_SET,
    "the second set begins where the machine reads the lower-case set"
);

/// The rows `from..to` filled.
const fn rows(from: usize, to: usize) -> Glyph {
    let mut glyph = [0; GLYPH];
    let mut row = from;
    while row < to {
        glyph[row] = 0xFF;
        row += 1;
    }
    glyph
}

/// The columns `from..to` filled.
const fn columns(from: u32, to: u32) -> Glyph {
    let mut bits = 0u8;
    let mut column = from;
    while column < to {
        bits |= 0x80 >> column;
        column += 1;
    }
    [bits; GLYPH]
}

const fn or(a: Glyph, b: Glyph) -> Glyph {
    let mut glyph = a;
    let mut row = 0;
    while row < GLYPH {
        glyph[row] |= b[row];
        row += 1;
    }
    glyph
}

const fn and(a: Glyph, b: Glyph) -> Glyph {
    let mut glyph = a;
    let mut row = 0;
    while row < GLYPH {
        glyph[row] &= b[row];
        row += 1;
    }
    glyph
}

const fn reverse(a: Glyph) -> Glyph {
    let mut glyph = a;
    let mut row = 0;
    while row < GLYPH {
        glyph[row] = !glyph[row];
        row += 1;
    }
    glyph
}

const BLANK: Glyph = [0; GLYPH];

/// Box drawing's strokes through the middle of the cell, and their halves,
/// each reaching the middle.
const ACROSS: Glyph = rows(3, 5);
const DOWN: Glyph = columns(3, 5);
const LEFT_HALF: Glyph = and(ACROSS, columns(0, 5));
const RIGHT_HALF: Glyph = and(ACROSS, columns(3, 8));
const UPPER_HALF: Glyph = and(DOWN, rows(0, 5));
const LOWER_HALF: Glyph = and(DOWN, rows(3, 8));

const UPPER_LEFT: Glyph = and(rows(0, 4), columns(0, 4));
const UPPER_RIGHT: Glyph = and(rows(0, 4), columns(4, 8));
const LOWER_LEFT: Glyph = and(rows(4, 8), columns(0, 4));
const LOWER_RIGHT: Glyph = and(rows(4, 8), columns(4, 8));

/// Every other pixel, alternating by row.
const SHADE: Glyph = [0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55];
/// Squares of two pixels.
const CHECKERS: Glyph = [0xCC, 0xCC, 0x33, 0x33, 0xCC, 0xCC, 0x33, 0x33];

#[rustfmt::skip]
const FALLING: Glyph = draw([
    "##......", ".##.....", "..##....", "...##...",
    "....##..", ".....##.", "......##", ".......#",
]);
#[rustfmt::skip]
const RISING: Glyph = draw([
    "......##", ".....##.", "....##..", "...##...",
    "..##....", ".##.....", "##......", "#.......",
]);
#[rustfmt::skip]
const FALLING_FILL: Glyph = draw([
    "#...#...", ".#...#..", "..#...#.", "...#...#",
    "#...#...", ".#...#..", "..#...#.", "...#...#",
]);
#[rustfmt::skip]
const RISING_FILL: Glyph = draw([
    "...#...#", "..#...#.", ".#...#..", "#...#...",
    "...#...#", "..#...#.", ".#...#..", "#...#...",
]);
#[rustfmt::skip]
const UPPER_LEFT_TRIANGLE: Glyph = draw([
    "########", "#######.", "######..", "#####...",
    "####....", "###.....", "##......", "#.......",
]);
#[rustfmt::skip]
const UPPER_RIGHT_TRIANGLE: Glyph = draw([
    "########", ".#######", "..######", "...#####",
    "....####", ".....###", "......##", ".......#",
]);
#[rustfmt::skip]
const ARC_DOWN_RIGHT: Glyph = draw([
    "........", "........", "........", ".....###",
    "....####", "...##...", "...##...", "...##...",
]);
#[rustfmt::skip]
const ARC_DOWN_LEFT: Glyph = draw([
    "........", "........", "........", "###.....",
    "####....", "...##...", "...##...", "...##...",
]);
#[rustfmt::skip]
const ARC_UP_RIGHT: Glyph = draw([
    "...##...", "...##...", "...##...", "....####",
    ".....###", "........", "........", "........",
]);
#[rustfmt::skip]
const ARC_UP_LEFT: Glyph = draw([
    "...##...", "...##...", "...##...", "####....",
    "###.....", "........", "........", "........",
]);
#[rustfmt::skip]
const SPADE: Glyph = draw([
    "...##...", "..####..", ".######.", "########",
    "########", "...##...", "..####..", "........",
]);
#[rustfmt::skip]
const HEART: Glyph = draw([
    ".##..##.", "########", "########", "########",
    ".######.", "..####..", "...##...", "........",
]);
#[rustfmt::skip]
const CLUB: Glyph = draw([
    "..####..", "..####..", "##.##.##", "########",
    "##.##.##", "...##...", "..####..", "........",
]);
#[rustfmt::skip]
const DIAMOND: Glyph = draw([
    "...##...", "..####..", ".######.", "########",
    ".######.", "..####..", "...##...", "........",
]);
#[rustfmt::skip]
const BULLET: Glyph = draw([
    "........", "..####..", ".######.", ".######.",
    ".######.", ".######.", "..####..", "........",
]);
#[rustfmt::skip]
const CIRCLE: Glyph = draw([
    "........", "..####..", ".##..##.", ".#....#.",
    ".#....#.", ".##..##.", "..####..", "........",
]);
#[rustfmt::skip]
const PI: Glyph = draw([
    "........", ".######.", ".######.", "..##.##.",
    "..##.##.", "..##.##.", ".##..##.", "........",
]);
#[rustfmt::skip]
const TICK: Glyph = draw([
    "........", ".......#", "......##", "#....##.",
    "##..##..", ".####...", "..##....", "........",
]);
#[rustfmt::skip]
const UP_ARROW: Glyph = draw([
    "...##...", "..####..", ".######.", "...##...",
    "...##...", "...##...", "...##...", "...##...",
]);
#[rustfmt::skip]
const LEFT_ARROW: Glyph = draw([
    "........", "..#.....", ".##.....", "########",
    "########", ".##.....", "..#.....", "........",
]);

/// Screen codes 64 to 95 of the upper-case set: PETSCII `$60–$7F`.
const GRAPHICS_60: [Glyph; 32] = [
    ACROSS,
    SPADE,
    columns(3, 4),
    rows(3, 4),
    rows(2, 3),
    rows(1, 2),
    rows(5, 6),
    columns(2, 3),
    columns(5, 6),
    ARC_DOWN_LEFT,
    ARC_UP_RIGHT,
    ARC_UP_LEFT,
    or(columns(0, 1), rows(7, 8)),
    FALLING,
    RISING,
    or(columns(0, 1), rows(0, 1)),
    or(columns(7, 8), rows(0, 1)),
    BULLET,
    rows(6, 7),
    HEART,
    columns(1, 2),
    ARC_DOWN_RIGHT,
    or(FALLING, RISING),
    CIRCLE,
    CLUB,
    columns(6, 7),
    DIAMOND,
    or(ACROSS, DOWN),
    and(SHADE, columns(0, 4)),
    DOWN,
    PI,
    UPPER_RIGHT_TRIANGLE,
];

/// Screen codes 96 to 127 of either set: PETSCII `$A0–$BF`.
const GRAPHICS_A0: [Glyph; 32] = [
    BLANK,
    columns(0, 4),
    rows(4, 8),
    rows(0, 1),
    rows(7, 8),
    columns(0, 1),
    SHADE,
    columns(7, 8),
    and(SHADE, rows(4, 8)),
    UPPER_LEFT_TRIANGLE,
    columns(6, 8),
    or(DOWN, RIGHT_HALF),
    LOWER_RIGHT,
    or(UPPER_HALF, RIGHT_HALF),
    or(LEFT_HALF, LOWER_HALF),
    rows(6, 8),
    or(RIGHT_HALF, LOWER_HALF),
    or(ACROSS, UPPER_HALF),
    or(ACROSS, LOWER_HALF),
    or(DOWN, LEFT_HALF),
    columns(0, 2),
    columns(0, 3),
    columns(5, 8),
    rows(0, 2),
    rows(0, 3),
    rows(5, 8),
    or(columns(7, 8), rows(7, 8)),
    LOWER_LEFT,
    UPPER_RIGHT,
    or(UPPER_HALF, LEFT_HALF),
    UPPER_LEFT,
    or(UPPER_LEFT, LOWER_RIGHT),
];

/// The MOS font's place for the pound sign.
const POUND: u8 = 96;

/// The MOS's glyph for `code`.
///
/// # Panics
///
/// At compile time, for a character the MOS does not draw.
const fn glyph(code: u8) -> Glyph {
    match bbc_glyph(code) {
        Some(glyph) => *glyph,
        None => panic!("the MOS draws characters 32 to 127"),
    }
}

/// Screen codes 0 to 63, the letters starting at `a`.
const fn text(code: u8, a: u8) -> Glyph {
    match code {
        0 => glyph(b'@'),
        1..=26 => glyph(a + code - 1),
        27 => glyph(b'['),
        28 => glyph(POUND),
        29 => glyph(b']'),
        30 => UP_ARROW,
        31 => LEFT_ARROW,
        _ => glyph(code),
    }
}

const fn upper_case(code: u8) -> Glyph {
    match code {
        0..=63 => text(code, b'A'),
        64..=95 => GRAPHICS_60[code as usize - 64],
        _ => GRAPHICS_A0[code as usize - 96],
    }
}

/// Lower case where the upper-case set has upper, and upper where it has
/// the graphics their letters' places held.
const fn lower_case(code: u8) -> Glyph {
    match code {
        0..=63 => text(code, b'a'),
        65..=90 => glyph(code - 65 + b'A'),
        94 => reverse(CHECKERS),
        95 => FALLING_FILL,
        105 => RISING_FILL,
        122 => TICK,
        _ => upper_case(code),
    }
}

const IMAGE: [u8; CHARACTER_BYTES] = {
    let mut rom = [0; CHARACTER_BYTES];
    let mut code = 0u8;
    while (code as usize) < SET_GLYPHS {
        let at = code as usize * GLYPH;
        let glyphs = [upper_case(code), lower_case(code)];
        let mut set = 0;
        while set < glyphs.len() {
            let reversed = reverse(glyphs[set]);
            let mut row = 0;
            while row < GLYPH {
                rom[set * SET_BYTES + at + row] = glyphs[set][row];
                rom[set * SET_BYTES + SET_GLYPHS * GLYPH + at + row] = reversed[row];
                row += 1;
            }
            set += 1;
        }
        code += 1;
    }
    rom
};

/// The character ROM.
pub fn build() -> Result<Image, AsmError> {
    Ok(Image {
        bytes: Vec::from(IMAGE),
        symbols: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(set: usize, code: usize) -> Glyph {
        let at = set * SET_BYTES + code * GLYPH;
        let mut glyph = BLANK;
        glyph.copy_from_slice(&IMAGE[at..at + GLYPH]);
        glyph
    }

    #[test]
    fn each_set_s_second_half_reverses_its_first() {
        assert_eq!(2 * SET_BYTES, CHARACTER_BYTES);
        for set in 0..2 {
            for code in 0..SET_GLYPHS {
                assert_eq!(shown(set, code + SET_GLYPHS), reverse(shown(set, code)));
            }
        }
    }

    #[test]
    fn the_letters_are_the_mos_s_and_the_sets_swap_their_case() {
        assert_eq!(shown(0, 1), glyph(b'A'));
        assert_eq!(shown(0, 26), glyph(b'Z'));
        assert_eq!(shown(1, 1), glyph(b'a'));
        assert_eq!(shown(1, 65), glyph(b'A'));
        assert_eq!(shown(1, 90), glyph(b'Z'));
        assert_eq!(shown(0, 48), glyph(b'0'));
        assert_eq!(shown(0, 28), glyph(POUND));
        assert_eq!(shown(0, 32), BLANK);
        for set in 0..2 {
            for code in (0..64).filter(|&code| code != 32) {
                assert_ne!(shown(set, code), BLANK, "set {set}'s screen code {code}");
            }
        }
    }

    #[test]
    fn box_drawing_strokes_meet_in_the_middle() {
        assert_eq!(shown(0, 91), or(ACROSS, DOWN), "┼");
        let corner = shown(0, 112);
        assert_eq!(corner[0], 0, "┌ has no foot above the middle");
        assert_eq!(
            (corner[3], corner[4]),
            (0x1F, 0x1F),
            "┌ reaches the right edge"
        );
        assert_eq!(corner[7], 0x18, "┌ reaches the bottom");
        assert_eq!(shown(0, 125)[0], 0x18, "┘ reaches the top");
        assert_eq!(shown(0, 125)[3], 0xF8, "┘ reaches the left edge");
    }

    #[test]
    fn blocks_fill_the_eighths_they_are_named_for() {
        assert_eq!(shown(0, 96), BLANK, "the shifted space");
        assert_eq!(shown(0, 97), [0xF0; GLYPH], "▌");
        assert_eq!(shown(0, 98), [0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF], "▄");
        assert_eq!(shown(0, 101), [0x80; GLYPH], "▏");
        assert_eq!(shown(0, 103), [0x01; GLYPH], "▕");
        assert_eq!(shown(0, 66), [0x10; GLYPH], "the fourth vertical eighth");
        assert_eq!(shown(0, 126), [0xF0, 0xF0, 0xF0, 0xF0, 0, 0, 0, 0], "▘");
    }

    #[test]
    fn the_lower_case_set_keeps_the_graphics_but_where_it_draws_its_own() {
        for code in 96..SET_GLYPHS {
            let own = matches!(code, 105 | 122);
            assert_eq!(shown(1, code) == shown(0, code), !own, "screen code {code}");
        }
        assert_eq!(shown(1, 122), TICK);
        assert_eq!(build().map(|image| image.bytes.len()), Ok(CHARACTER_BYTES));
    }
}
