//! The EGA's and VGA's CRT controller.
//!
//! The 6845's descendant: separate blanking and retrace timing, a line
//! compare that restarts the display (the split screen), a preset row scan,
//! double scanning, and byte, word and doubleword addressing with the CGA
//! and Hercules interleaves substituted into the address.

/// Which generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Generation {
    /// The EGA.
    Ega,
    /// The VGA.
    Vga,
}

/// Register numbers.
mod reg {
    pub const H_TOTAL: usize = 0x00;
    pub const H_DISPLAY_END: usize = 0x01;
    pub const H_BLANK_START: usize = 0x02;
    pub const H_BLANK_END: usize = 0x03;
    pub const H_RETRACE_START: usize = 0x04;
    pub const H_RETRACE_END: usize = 0x05;
    pub const V_TOTAL: usize = 0x06;
    pub const OVERFLOW: usize = 0x07;
    pub const PRESET_ROW: usize = 0x08;
    pub const MAX_SCAN: usize = 0x09;
    pub const CURSOR_START: usize = 0x0A;
    pub const CURSOR_END: usize = 0x0B;
    pub const START_HIGH: usize = 0x0C;
    pub const START_LOW: usize = 0x0D;
    pub const CURSOR_HIGH: usize = 0x0E;
    pub const CURSOR_LOW: usize = 0x0F;
    pub const V_RETRACE_START: usize = 0x10;
    pub const V_RETRACE_END: usize = 0x11;
    pub const V_DISPLAY_END: usize = 0x12;
    pub const OFFSET: usize = 0x13;
    pub const UNDERLINE: usize = 0x14;
    pub const V_BLANK_START: usize = 0x15;
    pub const V_BLANK_END: usize = 0x16;
    pub const MODE: usize = 0x17;
    pub const LINE_COMPARE: usize = 0x18;
}

/// Registers in the controller.
const REGISTERS: usize = 25;

/// What one character clock puts out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one of the controller's output signals"
)]
pub struct Tick {
    /// The plane address the sequencer fetches.
    pub addr: u16,
    /// The row scan: the scan line within the character row.
    pub row: u8,
    /// Display enable.
    pub display: bool,
    /// Blanking: neither display nor border is shown.
    pub blank: bool,
    /// The text cursor's output, before blinking.
    pub cursor: bool,
    /// Horizontal retrace begins on this clock.
    pub hsync: bool,
    /// Vertical retrace begins on this clock.
    pub vsync: bool,
    /// The line compare restarted the display on this line.
    pub split: bool,
}

/// A controller.
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one of the controller's flip-flops"
)]
pub struct Crtc {
    generation: Generation,
    index: u8,
    regs: [u8; REGISTERS],
    hc: u16,
    vc: u16,
    row: u8,
    /// Odd scan line of a double-scanned pair.
    second: bool,
    ma: u32,
    row_start: u32,
    hblank: bool,
    hretrace: bool,
    vblank: bool,
    vretrace: bool,
    split: bool,
    /// The frame counter that blinks the cursor and characters.
    fields: u32,
    /// The vertical interrupt is latched.
    interrupt: bool,
}

impl Crtc {
    /// A controller of `generation`, every register clear.
    #[must_use]
    pub const fn new(generation: Generation) -> Self {
        Self {
            generation,
            index: 0,
            regs: [0; REGISTERS],
            hc: 0,
            vc: 0,
            row: 0,
            second: false,
            ma: 0,
            row_start: 0,
            hblank: false,
            hretrace: false,
            vblank: false,
            vretrace: false,
            split: false,
            fields: 0,
            interrupt: false,
        }
    }

    /// Selects the register the data port reaches.
    pub fn select(&mut self, index: u8) {
        self.index = index & 0x3F;
    }

    /// The register the data port reaches.
    #[must_use]
    pub const fn selected(&self) -> u8 {
        self.index
    }

    /// Writes the selected register. On the VGA, registers 0–7 are locked
    /// while register 11h's protect bit is set, all but the line compare
    /// bit of the overflow.
    pub fn write(&mut self, value: u8) {
        let index = usize::from(self.index);
        if index >= REGISTERS {
            return;
        }
        let protected =
            self.generation == Generation::Vga && self.regs[reg::V_RETRACE_END] & 0x80 != 0;
        if protected && index < reg::OVERFLOW {
            return;
        }
        if protected && index == reg::OVERFLOW {
            self.regs[index] = (self.regs[index] & !0x10) | (value & 0x10);
            return;
        }
        if index == reg::V_RETRACE_END && value & 0x10 == 0 {
            self.interrupt = false;
        }
        self.regs[index] = value;
    }

    /// Reads the selected register. The VGA reads back every register; the
    /// EGA only the cursor location, its others reading as zero.
    #[must_use]
    pub fn read(&self) -> u8 {
        let index = usize::from(self.index);
        match (self.generation, index) {
            (_, i) if i >= REGISTERS => 0,
            (Generation::Ega, reg::CURSOR_HIGH | reg::CURSOR_LOW) | (Generation::Vga, _) => {
                self.regs[index]
            }
            (Generation::Ega, _) => 0,
        }
    }

    /// A register's value.
    #[must_use]
    pub const fn register(&self, index: usize) -> u8 {
        if index < REGISTERS {
            self.regs[index]
        } else {
            0
        }
    }

    fn vga(&self) -> bool {
        self.generation == Generation::Vga
    }

    /// A vertical value: its low eight bits and the overflow bits above.
    fn vertical(&self, low: usize, bit8: u8, bit9: Option<(usize, u8)>) -> u16 {
        let mut value = u16::from(self.regs[low]);
        if self.regs[reg::OVERFLOW] & bit8 != 0 {
            value |= 0x100;
        }
        if let (true, Some((register, bit))) = (self.vga(), bit9) {
            if self.regs[register] & bit != 0 {
                value |= 0x200;
            }
        }
        value
    }

    /// Character clocks in a line.
    #[must_use]
    pub fn line_clocks(&self) -> u16 {
        u16::from(self.regs[reg::H_TOTAL]) + if self.vga() { 5 } else { 2 }
    }

    /// Scan lines in a frame.
    #[must_use]
    pub fn frame_lines(&self) -> u16 {
        self.vertical(reg::V_TOTAL, 0x01, Some((reg::OVERFLOW, 0x20)))
            + if self.vga() { 2 } else { 1 }
    }

    fn display_end(&self) -> u16 {
        self.vertical(reg::V_DISPLAY_END, 0x02, Some((reg::OVERFLOW, 0x40)))
    }

    fn retrace_start(&self) -> u16 {
        self.vertical(reg::V_RETRACE_START, 0x04, Some((reg::OVERFLOW, 0x80)))
    }

    fn blank_start(&self) -> u16 {
        self.vertical(reg::V_BLANK_START, 0x08, Some((reg::MAX_SCAN, 0x20)))
    }

    fn line_compare(&self) -> u16 {
        self.vertical(reg::LINE_COMPARE, 0x10, Some((reg::MAX_SCAN, 0x40)))
    }

    fn h_blank_end(&self) -> u16 {
        let low = u16::from(self.regs[reg::H_BLANK_END] & 0x1F);
        if self.vga() && self.regs[reg::H_RETRACE_END] & 0x80 != 0 {
            low | 0x20
        } else {
            low
        }
    }

    fn h_blank_mask(&self) -> u16 {
        if self.vga() {
            0x3F
        } else {
            0x1F
        }
    }

    fn v_blank_mask(&self) -> u16 {
        if self.vga() {
            0x7F
        } else {
            0x1F
        }
    }

    fn max_scan(&self) -> u8 {
        self.regs[reg::MAX_SCAN] & 0x1F
    }

    fn double_scan(&self) -> bool {
        self.vga() && self.regs[reg::MAX_SCAN] & 0x80 != 0
    }

    /// The start address the next frame takes.
    fn start(&self) -> u32 {
        u32::from(crate::bits::word(
            self.regs[reg::START_LOW],
            self.regs[reg::START_HIGH],
        ))
    }

    /// How the character counter becomes a plane address: doubleword, word
    /// or byte mode, then the CGA and Hercules interleaves.
    fn address(&self, ma: u32, row: u8) -> u16 {
        let mode = self.regs[reg::MODE];
        let mut addr = if self.vga() && self.regs[reg::UNDERLINE] & 0x40 != 0 {
            (ma << 2) | ((ma >> 14) & 3)
        } else if mode & 0x40 == 0 {
            let wrap = if mode & 0x20 != 0 { ma >> 15 } else { ma >> 13 };
            (ma << 1) | (wrap & 1)
        } else {
            ma
        };
        if mode & 0x01 == 0 {
            addr = (addr & !0x2000) | (u32::from(row & 1) << 13);
        }
        if mode & 0x02 == 0 {
            addr = (addr & !0x4000) | (u32::from(row & 2) << 13);
        }
        crate::bits::low16(addr)
    }

    /// Whether the cursor shows on `row` of its cell. The VGA lights its
    /// start row to its end row, none if the end is above the start or bit 5
    /// turns it off. The EGA stops before its end row, so an end above the
    /// start splits the cursor round the rows between and an end past the
    /// cell lights all of it; it lights its start row alone when the end's
    /// low four bits name it, as IBM's card is measured to.
    fn cursor_lit(&self, row: u8) -> bool {
        let start = self.regs[reg::CURSOR_START];
        let first = start & 0x1F;
        let last = self.regs[reg::CURSOR_END] & 0x1F;
        match self.generation {
            Generation::Vga => start & 0x20 == 0 && (first..=last).contains(&row),
            Generation::Ega if first > self.max_scan() => false,
            Generation::Ega if last == first || last & 0x0F == first => row == first,
            Generation::Ega if first < last => {
                last > self.max_scan() || (first..last).contains(&row)
            }
            Generation::Ega => row >= first || row < last,
        }
    }

    /// Runs one character clock.
    pub fn step(&mut self) -> Tick {
        let hc = self.hc;
        let display_h = hc <= u16::from(self.regs[reg::H_DISPLAY_END]);
        let display_v = self.vc <= self.display_end();
        if hc == u16::from(self.regs[reg::H_BLANK_START]) {
            self.hblank = true;
        } else if self.hblank && hc & self.h_blank_mask() == self.h_blank_end() {
            self.hblank = false;
        }
        let hsync = hc == u16::from(self.regs[reg::H_RETRACE_START]) && !self.hretrace;
        if hsync {
            self.hretrace = true;
        } else if self.hretrace && hc & 0x1F == u16::from(self.regs[reg::H_RETRACE_END] & 0x1F) {
            self.hretrace = false;
        }
        let vsync = hc == 0 && self.vretrace_started();
        let display = display_h && display_v;
        let skew = u32::from((self.regs[reg::CURSOR_END] >> 5) & 3);
        let cursor_ma = u32::from(crate::bits::word(
            self.regs[reg::CURSOR_LOW],
            self.regs[reg::CURSOR_HIGH],
        )) + skew;
        let tick = Tick {
            addr: self.address(self.ma, self.row),
            row: self.row,
            display,
            blank: self.hblank || self.vblank,
            cursor: display && self.ma == cursor_ma && self.cursor_lit(self.row),
            hsync,
            vsync,
            split: self.split && hc == 0,
        };
        let count_by = self.count_by();
        if u32::from(hc) % count_by == count_by - 1 {
            self.ma = self.ma.wrapping_add(1) & 0xFFFF;
        }
        self.hc += 1;
        if self.hc >= self.line_clocks() {
            self.end_line();
        }
        tick
    }

    /// Whether vertical retrace began on this line, which latches the
    /// vertical interrupt when it is enabled.
    fn vretrace_started(&mut self) -> bool {
        let started = self.vc == self.retrace_start() && !self.vretrace;
        if started {
            self.vretrace = true;
            if self.regs[reg::V_RETRACE_END] & 0x20 == 0 {
                self.interrupt = true;
            }
        }
        started
    }

    fn end_line(&mut self) {
        self.hc = 0;
        self.split = false;
        // The split begins on the line after the one the counter matches.
        let compare = self.vc == self.line_compare();
        let last_row_line = self.row == self.max_scan() && (!self.double_scan() || self.second);
        if self.double_scan() && !self.second {
            self.second = true;
        } else {
            self.second = false;
            if last_row_line {
                self.row = 0;
                self.row_start = self
                    .row_start
                    .wrapping_add(u32::from(self.regs[reg::OFFSET]) * 2)
                    & 0xFFFF;
            } else {
                self.row = (self.row + 1) & 0x1F;
            }
        }
        self.vc += 1;
        if self.vc >= self.frame_lines() {
            self.begin_frame();
        }
        if compare {
            self.row_start = 0;
            self.row = 0;
            self.second = false;
            self.split = true;
        }
        if self.vc == self.blank_start() {
            self.vblank = true;
        } else if self.vblank
            && self.vc & self.v_blank_mask()
                == u16::from(self.regs[reg::V_BLANK_END]) & self.v_blank_mask()
        {
            self.vblank = false;
        }
        if self.vretrace && self.vc & 0x0F == u16::from(self.regs[reg::V_RETRACE_END] & 0x0F) {
            self.vretrace = false;
        }
        self.ma = self.line_counter();
    }

    /// The counters a frame begins with.
    fn begin_frame(&mut self) {
        self.vc = 0;
        self.row = self.regs[reg::PRESET_ROW] & 0x1F;
        self.second = false;
        self.row_start = self.start();
        self.fields = self.fields.wrapping_add(1);
    }

    /// The character counter a line begins at: its row's start, panned by
    /// whole bytes on the VGA.
    fn line_counter(&self) -> u32 {
        let pan = if self.vga() {
            u32::from((self.regs[reg::PRESET_ROW] >> 5) & 3)
        } else {
            0
        };
        self.row_start + pan
    }

    /// Character clocks each character counter is held for: doubleword
    /// mode's four, count by two's two, else one.
    fn count_by(&self) -> u32 {
        if self.vga() && self.regs[reg::UNDERLINE] & 0x20 != 0 {
            4
        } else if self.regs[reg::MODE] & 0x08 != 0 {
            2
        } else {
            1
        }
    }

    /// Character clocks a line displays.
    #[must_use]
    pub fn text_columns(&self) -> u16 {
        (u16::from(self.regs[reg::H_DISPLAY_END]) + 1).min(self.line_clocks())
    }

    /// The character counter and row scan each character row the next frame
    /// shows begins at, top first: a copy of the controller stepped down the
    /// displayed lines, so a preset row scan, double scanning and a split
    /// read as the beam draws them.
    pub fn text_rows(&self) -> impl Iterator<Item = (u32, u8)> {
        let mut walk = self.clone();
        walk.begin_frame();
        walk.ma = walk.line_counter();
        let last = self.display_end().min(self.frame_lines() - 1);
        (0..=last).filter_map(move |line| {
            let begins = line == 0 || (walk.row == 0 && !walk.second);
            let row = begins.then_some((walk.ma, walk.row));
            walk.end_line();
            row
        })
    }

    /// The plane address of character clock `column` of the row that
    /// [`Self::text_rows`] has begin at `counter` on row scan `scan`.
    #[must_use]
    pub fn text_address(&self, counter: u32, scan: u8, column: u16) -> u16 {
        let counter = counter.wrapping_add(u32::from(column) / self.count_by()) & 0xFFFF;
        self.address(counter, scan)
    }

    /// Whether display enable is off, in either direction: input status 1's
    /// bit 0.
    #[must_use]
    pub fn not_displaying(&self) -> bool {
        self.hc > u16::from(self.regs[reg::H_DISPLAY_END]) || self.vc > self.display_end()
    }

    /// Whether vertical retrace is under way: input status 1's bit 3.
    #[must_use]
    pub const fn in_vertical_retrace(&self) -> bool {
        self.vretrace
    }

    /// Whether the vertical interrupt is latched.
    #[must_use]
    pub const fn interrupt(&self) -> bool {
        self.interrupt
    }

    /// Character clocks to run until the one that begins vertical retrace
    /// has run, latching the vertical interrupt, if it is enabled.
    #[must_use]
    pub fn clocks_to_interrupt(&self) -> Option<u64> {
        if self.regs[reg::V_RETRACE_END] & 0x20 != 0 || self.interrupt {
            return None;
        }
        let line = u64::from(self.line_clocks());
        let lines = u64::from(self.frame_lines());
        let start = u64::from(self.retrace_start());
        if start >= lines {
            return None;
        }
        let now = u64::from(self.vc) * line + u64::from(self.hc);
        let at = start * line;
        Some(if at >= now {
            at - now + 1
        } else {
            at + lines * line - now + 1
        })
    }

    /// Frames begun, which time blinking.
    #[must_use]
    pub const fn fields(&self) -> u32 {
        self.fields
    }

    /// The underline's scan line.
    #[must_use]
    pub const fn underline_row(&self) -> u8 {
        self.regs[reg::UNDERLINE] & 0x1F
    }

    /// The visible window the timing implies, in character clocks and lines
    /// measured from the start of retrace: where blanking ends and how long
    /// until it begins again.
    #[must_use]
    pub fn visible(&self) -> (u32, u32, u32, u32) {
        let total = u32::from(self.line_clocks()).max(1);
        let lines = u32::from(self.frame_lines()).max(1);
        let hrs = u32::from(self.regs[reg::H_RETRACE_START]);
        let hbs = u32::from(self.regs[reg::H_BLANK_START]);
        let hbe = first_match(
            hbs,
            total,
            u32::from(self.h_blank_mask()),
            u32::from(self.h_blank_end()),
        );
        let vrs = u32::from(self.retrace_start());
        let vbs = u32::from(self.blank_start());
        let vbe = first_match(
            vbs,
            lines,
            u32::from(self.v_blank_mask()),
            u32::from(self.regs[reg::V_BLANK_END]) & u32::from(self.v_blank_mask()),
        );
        let left = (hbe + total - hrs) % total;
        let width = (hbs + total - hbe) % total;
        let top = (vbe + lines - vrs) % lines;
        let height = (vbs + lines - vbe) % lines;
        (left, width.max(1), top, height.max(1))
    }
}

/// The first count after `start`, wrapping at `total`, whose bits under
/// `mask` equal `end`: where a blanking interval programmed by its low bits
/// ends.
fn first_match(start: u32, total: u32, mask: u32, end: u32) -> u32 {
    (1..=total)
        .map(|i| (start + i) % total)
        .find(|&c| c & mask == end)
        .unwrap_or(start)
}

#[cfg(test)]
#[path = "crtc_tests.rs"]
pub(super) mod tests;

crate::snapshot::state!(Tick {
    addr,
    row,
    display,
    blank,
    cursor,
    hsync,
    vsync,
    split,
});

crate::snapshot::state!(
    Crtc {
        index,
        regs,
        hc,
        vc,
        row,
        second,
        ma,
        row_start,
        hblank,
        hretrace,
        vblank,
        vretrace,
        split,
        fields,
        interrupt,
    }
    as built { generation }
    check |crtc: &Crtc| crtc.index <= 0x3F
);
