//! The Teletext character generator the BBC Micro's SAA5050 is and RISC OS
//! draws mode 7 with: the alphanumeric glyphs, contiguous and separated
//! mosaics, character rounding to half-dot resolution, and the serial
//! attributes a row's control codes set as it is drawn.
//!
//! A cell is 6 × 10 dots, 12 × 20 half-dots once rounding fills in the
//! diagonals between alternate interlaced lines. The glyph shapes are
//! original to TAIRiX, drawn in the proportions of the chip's English
//! character set.

#![no_std]
#![forbid(unsafe_code)]

/// Half-dot lines per cell.
pub const HALF_LINES: usize = 20;
/// Half-dots across a cell: the leftmost in bit 11 of a line.
pub const HALF_DOTS: u32 = 12;

/// A cell's half-dot lines, the top first.
pub type Lines = [u16; HALF_LINES];

/// The cells there are: 96 alphanumerics, then 64 contiguous and 64
/// separated mosaics.
pub const CELLS: usize = SEPARATED + 64;
const CONTIGUOUS: usize = 96;
const SEPARATED: usize = 160;

/// The code that starts double height.
const DOUBLE_HEIGHT: u8 = 0x0D;

/// Whether `byte` is the double-height code, whatever its top bit: a row
/// holding one is the upper half of a double-height pair.
#[must_use]
pub const fn is_double_height(byte: u8) -> bool {
    byte & 0x7F == DOUBLE_HEIGHT
}

/// Build a glyph from ten rows of five columns, `#` for a dot.
const fn glyph(art: [&str; 10]) -> [u8; 10] {
    let mut rows = [0u8; 10];
    let mut row = 0;
    while row < 10 {
        let bytes = art[row].as_bytes();
        let mut column = 0;
        while column < 5 {
            if bytes[column] == b'#' {
                rows[row] |= 0x10 >> column;
            }
            column += 1;
        }
        row += 1;
    }
    rows
}

/// The alphanumeric set, codes `&20`–`&7F`; bit 4 of a row is its leftmost
/// dot, and the sixth column of the cell is always blank.
#[rustfmt::skip]
const ALPHANUMERICS: [[u8; 10]; 96] = [
    glyph([".....", ".....", ".....", ".....", ".....", ".....", ".....", ".....", ".....", "....."]),
    glyph([".....", "..#..", "..#..", "..#..", "..#..", "..#..", ".....", "..#..", ".....", "....."]),
    glyph([".....", ".#.#.", ".#.#.", ".#.#.", ".....", ".....", ".....", ".....", ".....", "....."]),
    glyph([".....", "..##.", ".#..#", ".#...", "###..", ".#...", ".#...", "#####", ".....", "....."]),
    glyph([".....", ".###.", "#.#.#", "#.#..", ".###.", "..#.#", "#.#.#", ".###.", ".....", "....."]),
    glyph([".....", "##...", "##..#", "...#.", "..#..", ".#...", "#..##", "...##", ".....", "....."]),
    glyph([".....", ".#...", "#.#..", "#.#..", ".#...", "#.#.#", "#..#.", ".##.#", ".....", "....."]),
    glyph([".....", "..#..", "..#..", ".#...", ".....", ".....", ".....", ".....", ".....", "....."]),
    glyph([".....", "...#.", "..#..", ".#...", ".#...", ".#...", "..#..", "...#.", ".....", "....."]),
    glyph([".....", ".#...", "..#..", "...#.", "...#.", "...#.", "..#..", ".#...", ".....", "....."]),
    glyph([".....", "..#..", "#.#.#", ".###.", "..#..", ".###.", "#.#.#", "..#..", ".....", "....."]),
    glyph([".....", ".....", "..#..", "..#..", "#####", "..#..", "..#..", ".....", ".....", "....."]),
    glyph([".....", ".....", ".....", ".....", ".....", ".....", "..#..", "..#..", ".#...", "....."]),
    glyph([".....", ".....", ".....", ".....", ".###.", ".....", ".....", ".....", ".....", "....."]),
    glyph([".....", ".....", ".....", ".....", ".....", ".....", ".....", "..#..", ".....", "....."]),
    glyph([".....", ".....", "....#", "...#.", "..#..", ".#...", "#....", ".....", ".....", "....."]),
    glyph([".....", "..#..", ".#.#.", "#...#", "#...#", "#...#", ".#.#.", "..#..", ".....", "....."]),
    glyph([".....", "..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###.", ".....", "....."]),
    glyph([".....", ".###.", "#...#", "....#", "..##.", ".#...", "#....", "#####", ".....", "....."]),
    glyph([".....", "#####", "....#", "...#.", "..##.", "....#", "#...#", ".###.", ".....", "....."]),
    glyph([".....", "...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#.", ".....", "....."]),
    glyph([".....", "#####", "#....", "####.", "....#", "....#", "#...#", ".###.", ".....", "....."]),
    glyph([".....", "..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###.", ".....", "....."]),
    glyph([".....", "#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#...", ".....", "....."]),
    glyph([".....", ".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###.", ".....", "....."]),
    glyph([".....", ".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##..", ".....", "....."]),
    glyph([".....", ".....", ".....", "..#..", ".....", ".....", ".....", "..#..", ".....", "....."]),
    glyph([".....", ".....", ".....", "..#..", ".....", ".....", "..#..", "..#..", ".#...", "....."]),
    glyph([".....", "...#.", "..#..", ".#...", "#....", ".#...", "..#..", "...#.", ".....", "....."]),
    glyph([".....", ".....", ".....", "#####", ".....", "#####", ".....", ".....", ".....", "....."]),
    glyph([".....", ".#...", "..#..", "...#.", "....#", "...#.", "..#..", ".#...", ".....", "....."]),
    glyph([".....", ".###.", "#...#", "...#.", "..#..", "..#..", ".....", "..#..", ".....", "....."]),
    glyph([".....", ".###.", "#...#", "#.###", "#.#.#", "#.###", "#....", ".###.", ".....", "....."]),
    glyph([".....", "..#..", ".#.#.", "#...#", "#...#", "#####", "#...#", "#...#", ".....", "....."]),
    glyph([".....", "####.", "#...#", "#...#", "####.", "#...#", "#...#", "####.", ".....", "....."]),
    glyph([".....", ".###.", "#...#", "#....", "#....", "#....", "#...#", ".###.", ".....", "....."]),
    glyph([".....", "####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####.", ".....", "....."]),
    glyph([".....", "#####", "#....", "#....", "####.", "#....", "#....", "#####", ".....", "....."]),
    glyph([".....", "#####", "#....", "#....", "####.", "#....", "#....", "#....", ".....", "....."]),
    glyph([".....", ".###.", "#...#", "#....", "#.###", "#...#", "#...#", ".####", ".....", "....."]),
    glyph([".....", "#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#", ".....", "....."]),
    glyph([".....", ".###.", "..#..", "..#..", "..#..", "..#..", "..#..", ".###.", ".....", "....."]),
    glyph([".....", "....#", "....#", "....#", "....#", "....#", "#...#", ".###.", ".....", "....."]),
    glyph([".....", "#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#", ".....", "....."]),
    glyph([".....", "#....", "#....", "#....", "#....", "#....", "#....", "#####", ".....", "....."]),
    glyph([".....", "#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#", ".....", "....."]),
    glyph([".....", "#...#", "#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#", ".....", "....."]),
    glyph([".....", ".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.", ".....", "....."]),
    glyph([".....", "####.", "#...#", "#...#", "####.", "#....", "#....", "#....", ".....", "....."]),
    glyph([".....", ".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#", ".....", "....."]),
    glyph([".....", "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#", ".....", "....."]),
    glyph([".....", ".###.", "#...#", "#....", ".###.", "....#", "#...#", ".###.", ".....", "....."]),
    glyph([".....", "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#..", ".....", "....."]),
    glyph([".....", "#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.", ".....", "....."]),
    glyph([".....", "#...#", "#...#", "#...#", ".#.#.", ".#.#.", "..#..", "..#..", ".....", "....."]),
    glyph([".....", "#...#", "#...#", "#...#", "#.#.#", "#.#.#", "#.#.#", ".#.#.", ".....", "....."]),
    glyph([".....", "#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#", ".....", "....."]),
    glyph([".....", "#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#..", ".....", "....."]),
    glyph([".....", "#####", "....#", "...#.", "..#..", ".#...", "#....", "#####", ".....", "....."]),
    glyph([".....", ".....", "..#..", ".#...", "#####", ".#...", "..#..", ".....", ".....", "....."]),
    glyph([".....", "#....", "#....", "#....", "..##.", "....#", "...#.", "..#..", "..###", "....."]),
    glyph([".....", ".....", "..#..", "...#.", "#####", "...#.", "..#..", ".....", ".....", "....."]),
    glyph([".....", "..#..", ".###.", "#.#.#", "..#..", "..#..", "..#..", "..#..", ".....", "....."]),
    glyph([".....", ".#.#.", ".#.#.", "#####", ".#.#.", "#####", ".#.#.", ".#.#.", ".....", "....."]),
    glyph([".....", ".....", ".....", ".....", "#####", ".....", ".....", ".....", ".....", "....."]),
    glyph([".....", ".....", ".....", ".###.", "....#", ".####", "#...#", ".####", ".....", "....."]),
    glyph([".....", "#....", "#....", "####.", "#...#", "#...#", "#...#", "####.", ".....", "....."]),
    glyph([".....", ".....", ".....", ".####", "#....", "#....", "#....", ".####", ".....", "....."]),
    glyph([".....", "....#", "....#", ".####", "#...#", "#...#", "#...#", ".####", ".....", "....."]),
    glyph([".....", ".....", ".....", ".###.", "#...#", "#####", "#....", ".###.", ".....", "....."]),
    glyph([".....", "..##.", ".#...", ".#...", "###..", ".#...", ".#...", ".#...", ".....", "....."]),
    glyph([".....", ".....", ".....", ".####", "#...#", "#...#", "#...#", ".####", "....#", ".###."]),
    glyph([".....", "#....", "#....", "####.", "#...#", "#...#", "#...#", "#...#", ".....", "....."]),
    glyph([".....", "..#..", ".....", ".##..", "..#..", "..#..", "..#..", ".###.", ".....", "....."]),
    glyph([".....", "...#.", ".....", "..##.", "...#.", "...#.", "...#.", "...#.", "...#.", ".##.."]),
    glyph([".....", ".#...", ".#...", ".#..#", ".#.#.", ".##..", ".#.#.", ".#..#", ".....", "....."]),
    glyph([".....", ".##..", "..#..", "..#..", "..#..", "..#..", "..#..", ".###.", ".....", "....."]),
    glyph([".....", ".....", ".....", "##.#.", "#.#.#", "#.#.#", "#.#.#", "#.#.#", ".....", "....."]),
    glyph([".....", ".....", ".....", "####.", "#...#", "#...#", "#...#", "#...#", ".....", "....."]),
    glyph([".....", ".....", ".....", ".###.", "#...#", "#...#", "#...#", ".###.", ".....", "....."]),
    glyph([".....", ".....", ".....", "####.", "#...#", "#...#", "#...#", "####.", "#....", "#...."]),
    glyph([".....", ".....", ".....", ".####", "#...#", "#...#", "#...#", ".####", "....#", "....#"]),
    glyph([".....", ".....", ".....", ".#.##", ".##..", ".#...", ".#...", ".#...", ".....", "....."]),
    glyph([".....", ".....", ".....", ".####", "#....", ".###.", "....#", "####.", ".....", "....."]),
    glyph([".....", ".#...", ".#...", "###..", ".#...", ".#...", ".#...", "..##.", ".....", "....."]),
    glyph([".....", ".....", ".....", "#...#", "#...#", "#...#", "#...#", ".####", ".....", "....."]),
    glyph([".....", ".....", ".....", "#...#", "#...#", ".#.#.", ".#.#.", "..#..", ".....", "....."]),
    glyph([".....", ".....", ".....", "#...#", "#...#", "#.#.#", "#.#.#", ".#.#.", ".....", "....."]),
    glyph([".....", ".....", ".....", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", ".....", "....."]),
    glyph([".....", ".....", ".....", "#...#", "#...#", "#...#", "#...#", ".####", "....#", ".###."]),
    glyph([".....", ".....", ".....", "#####", "...#.", "..#..", ".#...", "#####", ".....", "....."]),
    glyph([".....", "#....", "#....", "#....", "#..#.", "..##.", ".#.#.", ".####", "...#.", "....."]),
    glyph([".....", ".#.#.", ".#.#.", ".#.#.", ".#.#.", ".#.#.", ".#.#.", ".#.#.", ".....", "....."]),
    glyph([".....", "##...", "..#..", ".#...", "..#..", "##.#.", "..##.", ".#.#.", ".####", "...#."]),
    glyph([".....", ".....", "..#..", ".....", "#####", ".....", "..#..", ".....", ".....", "....."]),
    glyph([".....", "#####", "#####", "#####", "#####", "#####", "#####", "#####", "#####", "....."]),
];

/// Every cell, rounded, built when the program is.
pub static GLYPHS: [Lines; CELLS] = cells();

const fn cells() -> [Lines; CELLS] {
    let mut cells = [[0u16; HALF_LINES]; CELLS];
    let mut index = 0;
    while index < ALPHANUMERICS.len() {
        cells[index] = rounded(&ALPHANUMERICS[index]);
        index += 1;
    }
    let mut sextants = 0u8;
    while sextants < 64 {
        // Bit 5 of the cell index is the code's bit 6, the bottom-right
        // sextant; the code's own bit 5 only selects mosaics.
        let code = (sextants & 0x1F) | ((sextants & 0x20) << 1);
        cells[CONTIGUOUS + sextants as usize] = mosaic_lines(code, false);
        cells[SEPARATED + sextants as usize] = mosaic_lines(code, true);
        sextants += 1;
    }
    cells
}

/// Whether `glyph` has a dot at `row`, `column`; the sixth column never does.
const fn dot(glyph: &[u8; 10], row: usize, column: usize) -> bool {
    column < 5 && glyph[row] & (0x10 >> column) != 0
}

/// A glyph at half-dot resolution, rounded: a dot is two half-dots wide and
/// two lines tall, and where two dots touch only diagonally the half-dots
/// either side of the join fill in on the interlaced lines between them.
const fn rounded(glyph: &[u8; 10]) -> Lines {
    let mut lines = [0u16; HALF_LINES];
    let mut row = 0;
    while row < 10 {
        let mut column = 0;
        while column < 6 {
            if dot(glyph, row, column) {
                let dots = 0b11 << (10 - 2 * column);
                lines[2 * row] |= dots;
                lines[2 * row + 1] |= dots;
            }
            if row < 9 && column < 5 {
                let (here, right) = (dot(glyph, row, column), dot(glyph, row, column + 1));
                let (below, below_right) =
                    (dot(glyph, row + 1, column), dot(glyph, row + 1, column + 1));
                if here && below_right && !right && !below {
                    lines[2 * row + 1] |= 1 << (11 - (2 * column + 2));
                    lines[2 * row + 2] |= 1 << (11 - (2 * column + 1));
                }
                if right && below && !here && !below_right {
                    lines[2 * row + 1] |= 1 << (11 - (2 * column + 1));
                    lines[2 * row + 2] |= 1 << (11 - (2 * column + 2));
                }
            }
            column += 1;
        }
        row += 1;
    }
    lines
}

/// A mosaic: six blocks, two across and three down the 6 × 10 cell, split
/// three, four and three lines; separated blocks lose their left column and
/// bottom line.
const fn mosaic_lines(code: u8, separated: bool) -> Lines {
    const BANDS: [(usize, usize); 3] = [(0, 3), (3, 7), (7, 10)];
    const BITS: [u8; 6] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x40];
    let mut lines = [0u16; HALF_LINES];
    let mut block = 0;
    while block < 6 {
        let ((top, bottom), side) = (BANDS[block / 2], block % 2);
        if code & BITS[block] != 0 {
            let first = side * 3 + separated as usize;
            let last_row = bottom - 1 - separated as usize;
            let mut row = top;
            while row <= last_row {
                let mut column = first;
                while column <= side * 3 + 2 {
                    lines[2 * row] |= 0b11 << (10 - 2 * column);
                    lines[2 * row + 1] |= 0b11 << (10 - 2 * column);
                    column += 1;
                }
                row += 1;
            }
        }
        block += 1;
    }
    lines
}

/// The cell of alphanumeric `code`, `&20` to `&7F`.
#[must_use]
pub const fn alphanumeric(code: u8) -> usize {
    ((code & 0x7F).saturating_sub(0x20)) as usize
}

/// The cell of mosaic `code` (`&20`–`&3F`, `&60`–`&7F`).
#[must_use]
pub const fn mosaic(code: u8, separated: bool) -> usize {
    let sextants = ((code & 0x1F) | ((code & 0x40) >> 1)) as usize;
    if separated {
        SEPARATED + sextants
    } else {
        CONTIGUOUS + sextants
    }
}

/// What a row's control codes have set so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent latch the control codes set"
)]
pub struct Attributes {
    /// The foreground colour, 0 to 7.
    pub foreground: u8,
    /// The background colour.
    pub background: u8,
    /// Mosaics rather than alphanumerics.
    pub graphics: bool,
    /// Mosaics separated.
    pub separated: bool,
    /// Flashing.
    pub flash: bool,
    /// Double height.
    pub double: bool,
    /// Concealed until revealed.
    pub conceal: bool,
    /// Held mosaics: a control code shows the last mosaic.
    pub hold: bool,
    /// The last mosaic drawn since a change of mode or size, and whether it
    /// was separated.
    pub held: Option<(u8, bool)>,
}

/// One character as a row draws it: the cell shown, none for a blank, the
/// attributes it is shown with, and whether it is the double-height code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
    /// The cell, in [`GLYPHS`].
    pub cell: Option<usize>,
    /// The attributes in force as it is drawn.
    pub attributes: Attributes,
    /// It is the double-height code.
    pub double_height: bool,
}

impl Attributes {
    /// A row's attributes at its start: white alphanumerics on black.
    pub const LINE_START: Self = Self {
        foreground: 7,
        background: 0,
        graphics: false,
        separated: false,
        flash: false,
        double: false,
        conceal: false,
        hold: false,
        held: None,
    };

    /// The next character of the row, `byte`'s low seven bits: what it shows
    /// and with what, the attributes moved on past it.
    pub fn character(&mut self, byte: u8) -> Shown {
        let code = byte & 0x7F;
        let before = *self;
        if code < 0x20 {
            self.set_at(code);
        }
        let shown = *self;
        let cell = if code < 0x20 {
            match (shown.hold, shown.held) {
                (true, Some((held, separated))) => Some(mosaic(held, separated)),
                _ => None,
            }
        } else if shown.graphics && code & 0x20 != 0 {
            self.held = Some((code, shown.separated));
            Some(mosaic(code, shown.separated))
        } else {
            Some(alphanumeric(code))
        };
        if code < 0x20 {
            self.set_after(code, before);
        }
        Shown {
            cell,
            attributes: shown,
            double_height: is_double_height(code),
        }
    }

    /// Codes that take effect on their own cell.
    const fn set_at(&mut self, code: u8) {
        match code {
            0x09 => self.flash = false,
            0x0C => {
                if self.double {
                    self.held = None;
                }
                self.double = false;
            }
            0x18 => self.conceal = true,
            0x19 => self.separated = false,
            0x1A => self.separated = true,
            0x1C => self.background = 0,
            0x1D => self.background = self.foreground,
            0x1E => self.hold = true,
            _ => {}
        }
    }

    /// Codes that take effect from the next cell.
    const fn set_after(&mut self, code: u8, before: Self) {
        match code {
            0x01..=0x07 => {
                self.foreground = code;
                self.conceal = false;
                if before.graphics {
                    self.held = None;
                }
                self.graphics = false;
            }
            0x08 => self.flash = true,
            DOUBLE_HEIGHT => {
                if !self.double {
                    self.held = None;
                }
                self.double = true;
            }
            0x11..=0x17 => {
                self.foreground = code & 0x07;
                self.conceal = false;
                if !before.graphics {
                    self.held = None;
                }
                self.graphics = true;
            }
            0x1F => self.hold = false,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
