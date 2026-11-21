//! The SAA5050 teletext character generator.
//!
//! It receives one 7-bit byte per 1 MHz character and draws its cell from
//! `lib/teletext`'s glyphs, a line at a time across sixteen pixel slots, the
//! row's attributes moved on as each character is drawn. Its line counter is
//! its own: held at zero by VSYNC, stepped by every line's sync, ten lines to
//! a row.

use tairix_teletext::{self as teletext, Attributes, HALF_LINES};

use super::ula::SLOTS_PER_CYCLE;
use crate::machine::canvas::encode;

/// Pixel slots per character: one 1 MHz character, two of the video ULA's
/// 2 MHz cycles at 16 MHz.
pub const SLOTS: usize = 2 * SLOTS_PER_CYCLE;

/// Lines per character row each field draws.
const LINES_PER_ROW: u8 = 10;

/// Fields in a flash cycle, and how many of them show flashing characters.
const FLASH_PERIOD: u32 = 64;
const FLASH_SHOWN: u32 = 48;

/// Coverage of each of a character's sixteen slots, per half-dot line.
type Cell = [[u8; SLOTS]; HALF_LINES];

/// Every drawable cell as slots, built when the emulator is from
/// `lib/teletext`'s glyphs.
static CELLS: [Cell; teletext::CELLS] = cells();

#[expect(
    clippy::large_stack_arrays,
    reason = "run only by the compiler, to build the static table"
)]
const fn cells() -> [Cell; teletext::CELLS] {
    let mut cells = [[[0u8; SLOTS]; HALF_LINES]; teletext::CELLS];
    let mut index = 0;
    while index < teletext::CELLS {
        cells[index] = slots(&teletext::GLYPHS[index]);
        index += 1;
    }
    cells
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

    /// A displayed line ends, LOSE falling: count it, and at the end of a row
    /// decide whether the next is the lower half of a double-height pair.
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
        let teletext::Shown {
            cell,
            attributes: shown,
            double_height,
        } = self.attributes.character(byte);
        if double_height {
            self.double_seen = true;
        }
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
