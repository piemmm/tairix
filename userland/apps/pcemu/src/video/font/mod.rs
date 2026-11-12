//! The character generators: first-party code page 437 at 8×8, 8×14 and
//! 8×16.
//!
//! The sets are reviewable text, `cp437-8x8.txt` and `cp437-8x14.txt`,
//! parsed when the crate compiles, so a malformed glyph is a build error.
//! The 8×16 set is the 8×14 one with a row added above and below: dark for
//! text, the pieces that join their neighbours carried to the cell's edge,
//! and the shades continuing their pattern.

/// A character set: 256 glyphs `H` rows tall, each row's leftmost dot in
/// bit 7.
#[derive(Debug)]
pub struct Font<const H: usize>([[u8; H]; 256]);

impl<const H: usize> Font<H> {
    /// Rows in a glyph.
    pub const HEIGHT: usize = H;

    /// Glyph `code`'s row `row`; rows below the glyph are dark.
    #[must_use]
    pub const fn row(&self, code: u8, row: usize) -> u8 {
        if row < H {
            self.0[code as usize][row]
        } else {
            0
        }
    }

    /// Glyph `code`.
    #[must_use]
    pub const fn glyph(&self, code: u8) -> &[u8; H] {
        &self.0[code as usize]
    }
}

/// Whether a nine-dot cell repeats glyph `code`'s eighth column in its
/// ninth, as the MDA and VGA do for the line and block pieces so they join.
#[must_use]
pub const fn joins_ninth_column(code: u8) -> bool {
    matches!(code, 0xC0..=0xDF)
}

/// The CGA's set, and the EGA's and VGA's for 200-line text.
pub static CGA: Font<8> = Font(parse(include_bytes!("cp437-8x8.txt")));
/// The MDA's and the EGA's set.
pub static EGA: Font<14> = Font(EGA_GLYPHS);
/// The VGA's set.
pub static VGA: Font<16> = Font(extend(&EGA_GLYPHS));

const EGA_GLYPHS: [[u8; 14]; 256] = parse(include_bytes!("cp437-8x14.txt"));

/// Where the line starting at `at` ends.
const fn line_end(source: &[u8], at: usize) -> usize {
    let mut end = at;
    while end < source.len() && source[end] != b'\n' {
        end += 1;
    }
    end
}

const fn hex_digit(byte: u8) -> usize {
    match byte {
        b'0'..=b'9' => (byte - b'0') as usize,
        b'A'..=b'F' => (byte - b'A' + 10) as usize,
        _ => panic!("a glyph's code is not two upper-case hex digits"),
    }
}

/// The eight dots of the row at `at`.
const fn dots(source: &[u8], at: usize) -> u8 {
    let mut row = 0u8;
    let mut i = 0;
    while i < 8 {
        row <<= 1;
        match source[at + i] {
            b'#' => row |= 1,
            b'.' => {}
            _ => panic!("a glyph row holds something other than '#' and '.'"),
        }
        i += 1;
    }
    row
}

/// Parses a set: every one of the 256 glyphs exactly once, each `H` rows of
/// eight dots.
const fn parse<const H: usize>(source: &[u8]) -> [[u8; H]; 256] {
    let mut glyphs = [[0u8; H]; 256];
    let mut seen = [false; 256];
    let mut at = 0;
    while at < source.len() {
        let end = line_end(source, at);
        if end > at && source[at] == b'@' {
            assert!(end >= at + 3, "a glyph header is too short");
            let code = hex_digit(source[at + 1]) * 16 + hex_digit(source[at + 2]);
            assert!(!seen[code], "a glyph is defined twice");
            seen[code] = true;
            let mut row = 0;
            at = end + 1;
            while row < H {
                assert!(at < source.len(), "a glyph is cut short");
                let row_end = line_end(source, at);
                assert!(row_end - at == 8, "a glyph row is not eight dots");
                glyphs[code][row] = dots(source, at);
                at = row_end + 1;
                row += 1;
            }
        } else {
            assert!(
                end == at || source[at] == b';',
                "a line is neither a glyph, a comment nor blank"
            );
            at = end + 1;
        }
    }
    let mut code = 0;
    while code < 256 {
        assert!(seen[code], "a glyph is missing");
        code += 1;
    }
    glyphs
}

/// Whether glyph `code` reaches the cell's top and bottom edges, so the
/// taller cell carries it on.
const fn joins_vertically(code: usize) -> bool {
    matches!(code, 0x08 | 0x0A | 0xB3..=0xDF | 0xF4 | 0xF5)
}

/// The 8×16 set from the 8×14 one.
const fn extend(glyphs: &[[u8; 14]; 256]) -> [[u8; 16]; 256] {
    let mut out = [[0u8; 16]; 256];
    let mut code = 0;
    while code < 256 {
        let glyph = &glyphs[code];
        let mut row = 0;
        while row < 16 {
            out[code][row] = if matches!(code, 0xB0..=0xB2) {
                glyph[row % 4]
            } else if row == 0 {
                if joins_vertically(code) {
                    glyph[0]
                } else {
                    0
                }
            } else if row == 15 {
                if joins_vertically(code) {
                    glyph[13]
                } else {
                    0
                }
            } else {
                glyph[row - 1]
            };
            row += 1;
        }
        code += 1;
    }
    out
}

#[cfg(test)]
mod tests;
