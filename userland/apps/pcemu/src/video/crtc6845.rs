//! The Motorola 6845 CRT controller, as the MDA, Hercules and CGA use it.
//!
//! The controller is a set of counters stepped once per character clock.
//! [`Crtc::step`] runs one clock and reports what the card needs from it:
//! the memory and row addresses, whether display and the cursor are on, and
//! where the sync pulses begin.

/// Register numbers.
mod reg {
    pub const H_TOTAL: usize = 0;
    pub const H_DISPLAYED: usize = 1;
    pub const H_SYNC: usize = 2;
    pub const SYNC_WIDTH: usize = 3;
    pub const V_TOTAL: usize = 4;
    pub const V_ADJUST: usize = 5;
    pub const V_DISPLAYED: usize = 6;
    pub const V_SYNC: usize = 7;
    pub const MAX_SCAN: usize = 9;
    pub const CURSOR_START: usize = 10;
    pub const CURSOR_END: usize = 11;
    pub const START_HIGH: usize = 12;
    pub const START_LOW: usize = 13;
    pub const CURSOR_HIGH: usize = 14;
    pub const CURSOR_LOW: usize = 15;
    pub const PEN_HIGH: usize = 16;
    pub const PEN_LOW: usize = 17;
}

/// The bits each register holds.
const WIDTH: [u8; 18] = [
    0xFF, 0xFF, 0xFF, 0x0F, 0x7F, 0x1F, 0x7F, 0x7F, 0x03, 0x1F, 0x7F, 0x1F, 0x3F, 0xFF, 0x3F, 0xFF,
    0x3F, 0xFF,
];

/// Lines the 6845's vertical sync lasts: fixed, not programmable.
const VSYNC_LINES: u8 = 16;

/// What one character clock puts out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one of the controller's output signals"
)]
pub struct Tick {
    /// The memory address (MA0–MA13).
    pub ma: u16,
    /// The row address (RA0–RA4): the scan line within the character row.
    pub ra: u8,
    /// Display enable.
    pub display: bool,
    /// The cursor's output, before the card's own blinking.
    pub cursor: bool,
    /// Horizontal sync begins on this clock.
    pub hsync: bool,
    /// Vertical sync begins on this clock.
    pub vsync: bool,
    /// Horizontal sync is asserted.
    pub in_hsync: bool,
    /// Vertical sync is asserted.
    pub in_vsync: bool,
}

/// The vertical counters' phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Vertical {
    /// Counting character rows.
    Rows,
    /// The total-adjust lines after the last row, this many done.
    Adjust(u8),
}

/// A 6845.
#[derive(Clone, Debug)]
pub struct Crtc {
    index: u8,
    regs: [u8; 18],
    hc: u8,
    ra: u8,
    vc: u8,
    vertical: Vertical,
    ma: u16,
    row_start: u16,
    /// MA as the displayed part of the last row's line ended: the next row's
    /// start.
    row_end: u16,
    hsync_left: u8,
    vsync_left: u8,
    display_v: bool,
    /// Frames begun, for the cursor's blink.
    fields: u32,
}

impl Default for Crtc {
    fn default() -> Self {
        Self::new()
    }
}

impl Crtc {
    /// A controller with every register clear, as power-up leaves it.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            index: 0,
            regs: [0; 18],
            hc: 0,
            ra: 0,
            vc: 0,
            vertical: Vertical::Rows,
            ma: 0,
            row_start: 0,
            row_end: 0,
            hsync_left: 0,
            vsync_left: 0,
            display_v: true,
            fields: 0,
        }
    }

    /// Selects the register the data port reaches.
    pub fn select(&mut self, index: u8) {
        self.index = index & 0x1F;
    }

    /// Writes the selected register; the light pen's two are read-only.
    pub fn write(&mut self, value: u8) {
        let index = usize::from(self.index);
        if index < reg::PEN_HIGH {
            self.regs[index] = value & WIDTH[index];
        }
    }

    /// Reads the selected register: the cursor address and the light pen
    /// read back; every other register reads as zero.
    #[must_use]
    pub fn read(&self) -> u8 {
        let index = usize::from(self.index);
        if (reg::CURSOR_HIGH..=reg::PEN_LOW).contains(&index) {
            self.regs[index]
        } else {
            0
        }
    }

    /// Latches the current memory address into the light pen registers.
    pub fn strobe_light_pen(&mut self) {
        let [low, high] = self.ma.to_le_bytes();
        self.regs[reg::PEN_HIGH] = high & WIDTH[reg::PEN_HIGH];
        self.regs[reg::PEN_LOW] = low;
    }

    /// A register's value.
    #[must_use]
    pub const fn register(&self, index: usize) -> u8 {
        if index < self.regs.len() {
            self.regs[index]
        } else {
            0
        }
    }

    fn word(&self, high: usize, low: usize) -> u16 {
        crate::bits::word(self.regs[low], self.regs[high])
    }

    /// The characters a frame shows: columns a row, and rows. A count past
    /// its total is met only as far as the total.
    #[must_use]
    pub fn text_shape(&self) -> (u16, u16) {
        let reaches = |displayed: usize, total: usize| {
            u16::from(self.regs[displayed]).min(u16::from(self.regs[total]) + 1)
        };
        (
            reaches(reg::H_DISPLAYED, reg::H_TOTAL),
            reaches(reg::V_DISPLAYED, reg::V_TOTAL),
        )
    }

    /// The address character `column` of row `row` is read from. Each row
    /// begins where the last row's displayed part ended, so rows advance by
    /// the displayed count, or not at all when the line ends before it.
    #[must_use]
    pub fn cell_address(&self, row: u16, column: u16) -> u16 {
        let displayed = u16::from(self.regs[reg::H_DISPLAYED]);
        let advance = if displayed <= u16::from(self.regs[reg::H_TOTAL]) + 1 {
            u32::from(displayed)
        } else {
            0
        };
        let start = u32::from(self.word(reg::START_HIGH, reg::START_LOW));
        crate::bits::low16((start + u32::from(row) * advance + u32::from(column)) & 0x3FFF)
    }

    /// Whether the cursor shows on row address `ra` at this field: the
    /// start and end lines, wrapping when the start is below the end, and
    /// the blink R10 asks for.
    fn cursor_lit(&self, ra: u8) -> bool {
        let start = self.regs[reg::CURSOR_START];
        let first = start & 0x1F;
        let last = self.regs[reg::CURSOR_END];
        let lines = if first <= last {
            (first..=last).contains(&ra)
        } else {
            ra >= first || ra <= last
        };
        lines
            && match start >> 5 {
                0 => true,
                1 => false,
                2 => self.fields & 0x08 != 0,
                _ => self.fields & 0x10 != 0,
            }
    }

    /// Runs one character clock.
    pub fn step(&mut self) -> Tick {
        let hc = self.hc;
        let display = hc < self.regs[reg::H_DISPLAYED] && self.display_v;
        let hsync = hc == self.regs[reg::H_SYNC] && self.regs[reg::SYNC_WIDTH] != 0;
        if hsync {
            self.hsync_left = self.regs[reg::SYNC_WIDTH];
        }
        let vsync = hc == 0 && self.vsync_left == VSYNC_LINES;
        let tick = Tick {
            ma: self.ma,
            ra: self.ra,
            display,
            cursor: display
                && self.ma == self.word(reg::CURSOR_HIGH, reg::CURSOR_LOW)
                && self.cursor_lit(self.ra),
            hsync,
            vsync,
            in_hsync: self.hsync_left > 0,
            in_vsync: self.vsync_left > 0,
        };
        self.hsync_left = self.hsync_left.saturating_sub(1);
        if hc.wrapping_add(1) == self.regs[reg::H_DISPLAYED] && self.ra == self.regs[reg::MAX_SCAN]
        {
            self.row_end = self.ma.wrapping_add(1) & 0x3FFF;
        }
        self.ma = self.ma.wrapping_add(1) & 0x3FFF;
        if hc == self.regs[reg::H_TOTAL] {
            self.end_line();
        } else {
            // An 8-bit counter: a total reprogrammed below it is met again
            // after it wraps.
            self.hc = self.hc.wrapping_add(1);
        }
        tick
    }

    /// The horizontal counter passed its total: the next scan line, row or
    /// frame begins.
    fn end_line(&mut self) {
        self.hc = 0;
        self.vsync_left = self.vsync_left.saturating_sub(1);
        let last_row = self.vc == self.regs[reg::V_TOTAL];
        match self.vertical {
            Vertical::Rows if self.ra == self.regs[reg::MAX_SCAN] => {
                self.ra = 0;
                self.row_start = self.row_end;
                if last_row {
                    if self.regs[reg::V_ADJUST] == 0 {
                        self.start_frame();
                        return;
                    }
                    self.vertical = Vertical::Adjust(0);
                } else {
                    self.vc = (self.vc + 1) & 0x7F;
                    self.begin_row();
                }
            }
            Vertical::Rows => self.ra = (self.ra + 1) & 0x1F,
            Vertical::Adjust(done) => {
                if done + 1 >= self.regs[reg::V_ADJUST] {
                    self.start_frame();
                    return;
                }
                self.vertical = Vertical::Adjust(done + 1);
                self.ra = (self.ra + 1) & 0x1F;
            }
        }
        self.ma = self.row_start;
    }

    fn start_frame(&mut self) {
        self.vertical = Vertical::Rows;
        self.vc = 0;
        self.ra = 0;
        self.fields = self.fields.wrapping_add(1);
        self.row_start = self.word(reg::START_HIGH, reg::START_LOW);
        self.row_end = self.row_start;
        self.ma = self.row_start;
        self.begin_row();
    }

    /// A character row begins: vertical display and sync are decided.
    fn begin_row(&mut self) {
        if self.vc == 0 {
            self.display_v = true;
        }
        if self.vc == self.regs[reg::V_DISPLAYED] {
            self.display_v = false;
        }
        if self.vc == self.regs[reg::V_SYNC] && self.vsync_left == 0 {
            self.vsync_left = VSYNC_LINES;
        }
    }

    /// The character clocks a line takes, as programmed.
    #[must_use]
    pub fn line_clocks(&self) -> u32 {
        u32::from(self.regs[reg::H_TOTAL]) + 1
    }

    /// Whether the controller is outside its displayed area: the horizontal
    /// and vertical blanking a status port reports.
    #[must_use]
    pub fn blanking(&self) -> bool {
        self.hc >= self.regs[reg::H_DISPLAYED] || !self.display_v
    }

    /// Whether vertical sync is asserted.
    #[must_use]
    pub const fn in_vsync(&self) -> bool {
        self.vsync_left > 0
    }
}

crate::snapshot::state!(Tick {
    ma,
    ra,
    display,
    cursor,
    hsync,
    vsync,
    in_hsync,
    in_vsync,
});

crate::snapshot::state_enum!(Vertical {
    0 => Rows,
    1 => Adjust(lines),
});

crate::snapshot::state!(
    Crtc {
        index,
        regs,
        hc,
        ra,
        vc,
        vertical,
        ma,
        row_start,
        row_end,
        hsync_left,
        vsync_left,
        display_v,
        fields,
    }
    check |crtc: &Crtc| crtc.index <= 0x1F
);

#[cfg(test)]
#[path = "crtc6845_tests.rs"]
mod tests;
