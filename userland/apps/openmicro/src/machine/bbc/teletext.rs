//! The SAA5050 teletext character generator.
//!
//! It receives one 7-bit byte per 1 MHz character and draws a 6 × 10 dot
//! cell, 12 × 20 half-dots once character rounding fills in the diagonals
//! between alternate interlaced lines. The attribute codes in the data change
//! colour, mosaic graphics, flashing, double height, concealment and held
//! mosaics as the line is drawn. Its line counter is its own: held at zero by
//! VSYNC, stepped by every line's sync, ten lines to a row.
//!
//! The glyph shapes are original to this emulator, drawn in the proportions of
//! the chip's English character set.

use super::ula::SLOTS_PER_CYCLE;
use crate::machine::canvas::encode;

/// Pixel slots per character: one 1 MHz character, two of the video ULA's
/// 2 MHz cycles at 16 MHz.
pub const SLOTS: usize = 2 * SLOTS_PER_CYCLE;

/// Half-dot lines per character cell.
const HALF_LINES: usize = 20;

/// Lines per character row each field draws.
const LINES_PER_ROW: u8 = 10;

/// Fields in a flash cycle, and how many of them show flashing characters.
const FLASH_PERIOD: u32 = 64;
const FLASH_SHOWN: u32 = 48;

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

/// Coverage of each of a character's sixteen slots, per half-dot line.
type Cell = [[u8; SLOTS]; HALF_LINES];

const CONTIGUOUS: usize = 96;
const SEPARATED: usize = 160;

/// Every drawable cell: 96 alphanumerics, then 64 contiguous and 64
/// separated mosaics, built when the emulator is.
static CELLS: [Cell; SEPARATED + 64] = cells();

#[expect(
    clippy::large_stack_arrays,
    reason = "run only by the compiler, to build the static table"
)]
const fn cells() -> [Cell; SEPARATED + 64] {
    let mut cells = [[[0u8; SLOTS]; HALF_LINES]; SEPARATED + 64];
    let mut index = 0;
    while index < ALPHANUMERICS.len() {
        cells[index] = slots(&rounded(&ALPHANUMERICS[index]));
        index += 1;
    }
    let mut sextants = 0u8;
    while sextants < 64 {
        // Bit 5 of the cell index is the code's bit 6, the bottom-right
        // sextant; the code's own bit 5 only selects mosaics.
        let code = (sextants & 0x1F) | ((sextants & 0x20) << 1);
        cells[CONTIGUOUS + sextants as usize] = slots(&mosaic(code, false));
        cells[SEPARATED + sextants as usize] = slots(&mosaic(code, true));
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
const fn rounded(glyph: &[u8; 10]) -> [u16; HALF_LINES] {
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
const fn mosaic(code: u8, separated: bool) -> [u16; HALF_LINES] {
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

/// Spread twelve half-dots over sixteen slots by coverage: each slot covers
/// three quarters of a half-dot, so its coverage is a whole number of thirds.
const fn slots(lines: &[u16; HALF_LINES]) -> Cell {
    let mut cell = [[0u8; SLOTS]; HALF_LINES];
    let mut line = 0;
    while line < HALF_LINES {
        let mut slot = 0;
        while slot < SLOTS {
            // The slot spans quarter-dots [3s, 3s + 3); half-dot h spans
            // [4h, 4h + 4).
            let mut quarter = 3 * slot;
            while quarter < 3 * slot + 3 {
                cell[line][slot] += (lines[line] >> (11 - quarter / 4) & 1) as u8;
                quarter += 1;
            }
            slot += 1;
        }
        line += 1;
    }
    cell
}

/// What the attribute codes have set so far on the current line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent latch the control codes set"
)]
struct Attributes {
    foreground: u8,
    background: u8,
    graphics: bool,
    separated: bool,
    flash: bool,
    double: bool,
    conceal: bool,
    hold: bool,
    /// The last mosaic drawn since a change of mode or size, and whether it
    /// was separated.
    held: Option<(u8, bool)>,
}

impl Attributes {
    const LINE_START: Self = Self {
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
}

/// The chip.
pub struct Saa5050 {
    attributes: Attributes,
    /// The line within the current character row, `0..10`.
    line: u8,
    /// The current row is the lower half of a double-height pair.
    lower_row: bool,
    /// A double-height code has been seen on the current row.
    double_seen: bool,
    /// VSYNC (data entry window) is active.
    dew: bool,
    /// Fields counted for the flash cycle.
    fields: u32,
}

impl Default for Saa5050 {
    fn default() -> Self {
        Self::new()
    }
}

impl Saa5050 {
    /// A chip at power-on.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            attributes: Attributes::LINE_START,
            line: 0,
            lower_row: false,
            double_seen: false,
            dew: false,
            fields: 0,
        }
    }

    /// VSYNC moves: while it is active the counters are held at the top of a
    /// field; each rise counts a field for flashing.
    pub fn set_dew(&mut self, active: bool) {
        if active && !self.dew {
            self.fields = self.fields.wrapping_add(1);
        }
        self.dew = active;
        if active {
            self.line = 0;
            self.lower_row = false;
            self.double_seen = false;
        }
    }

    /// A line starts being displayed: attributes return to their defaults.
    pub fn start_line(&mut self) {
        self.attributes = Attributes::LINE_START;
    }

    /// A line's sync: count it, and at the end of a row decide whether the
    /// next is the lower half of a double-height pair.
    pub fn end_line(&mut self) {
        if self.dew {
            return;
        }
        self.line += 1;
        if self.line == LINES_PER_ROW {
            self.line = 0;
            self.lower_row = self.double_seen && !self.lower_row;
            self.double_seen = false;
        }
    }

    /// Draw one character: `byte` from screen memory, `rounding_line` the
    /// interlaced half of the line (RA0), into sixteen encoded slots.
    pub fn character(&mut self, byte: u8, rounding_line: bool, out: &mut [u8; SLOTS]) {
        let code = byte & 0x7F;
        let before = self.attributes;
        if code < 0x20 {
            self.set_at(code);
        }
        let shown = self.attributes;
        let cell = if code < 0x20 {
            match (shown.hold, shown.held) {
                (true, Some((held, separated))) => Some(mosaic_cell(held, separated)),
                _ => None,
            }
        } else if shown.graphics && code & 0x20 != 0 {
            self.attributes.held = Some((code, shown.separated));
            Some(mosaic_cell(code, shown.separated))
        } else {
            Some(usize::from(code - 0x20))
        };
        let flash_hidden = shown.flash && self.fields % FLASH_PERIOD >= FLASH_SHOWN;
        let visible = !(shown.conceal || flash_hidden || (self.lower_row && !shown.double));
        let line = self.half_line(shown.double, rounding_line);
        match cell.filter(|_| visible) {
            Some(index) => {
                let coverage = &CELLS[index][line];
                for (slot, &c) in out.iter_mut().zip(coverage) {
                    *slot = encode(shown.foreground, shown.background, c);
                }
            }
            None => out.fill(encode(shown.background, shown.background, 3)),
        }
        if code < 0x20 {
            self.set_after(code, before);
        }
    }

    /// The half-dot line of the cell this line draws.
    fn half_line(&self, double: bool, rounding_line: bool) -> usize {
        let line = usize::from(self.line);
        if double {
            let half = if self.lower_row { 10 } else { 0 };
            half + line
        } else {
            2 * line + usize::from(rounding_line)
        }
    }

    /// Codes that take effect on their own cell.
    fn set_at(&mut self, code: u8) {
        let a = &mut self.attributes;
        match code {
            0x09 => a.flash = false,
            0x0C => {
                if a.double {
                    a.held = None;
                }
                a.double = false;
            }
            0x18 => a.conceal = true,
            0x19 => a.separated = false,
            0x1A => a.separated = true,
            0x1C => a.background = 0,
            0x1D => a.background = a.foreground,
            0x1E => a.hold = true,
            _ => {}
        }
    }

    /// Codes that take effect from the next cell.
    fn set_after(&mut self, code: u8, before: Attributes) {
        let a = &mut self.attributes;
        match code {
            0x01..=0x07 => {
                a.foreground = code;
                a.conceal = false;
                if before.graphics {
                    a.held = None;
                }
                a.graphics = false;
            }
            0x08 => a.flash = true,
            0x0D => {
                if !a.double {
                    a.held = None;
                }
                a.double = true;
                self.double_seen = true;
            }
            0x11..=0x17 => {
                a.foreground = code & 0x07;
                a.conceal = false;
                if !before.graphics {
                    a.held = None;
                }
                a.graphics = true;
            }
            0x1F => a.hold = false,
            _ => {}
        }
    }
}

crate::snapshot::fields!(
    Attributes {
        foreground,
        background,
        graphics,
        separated,
        flash,
        double,
        conceal,
        hold,
        held,
    },
    check | a | (1..=7).contains(&a.foreground)
        && a.background <= 7
        && a.held
            .is_none_or(|(code, _)| code < 0x80 && code & 0x20 != 0)
);

impl crate::snapshot::Save for Saa5050 {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        w.put(&self.attributes);
        w.put(&self.line);
        w.put(&self.lower_row);
        w.put(&self.double_seen);
        w.put(&self.dew);
        w.put(&self.fields);
    }
}

impl crate::snapshot::Load for Saa5050 {
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        let chip = Self {
            attributes: r.get()?,
            line: r.get()?,
            lower_row: r.get()?,
            double_seen: r.get()?,
            dew: r.get()?,
            fields: r.get()?,
        };
        crate::snapshot::Reader::check(chip.line < LINES_PER_ROW)?;
        Ok(chip)
    }
}

/// The cell index of a mosaic code (`&20`–`&3F`, `&60`–`&7F`).
fn mosaic_cell(code: u8, separated: bool) -> usize {
    let sextants = usize::from((code & 0x1F) | ((code & 0x40) >> 1));
    if separated {
        SEPARATED + sextants
    } else {
        CONTIGUOUS + sextants
    }
}
