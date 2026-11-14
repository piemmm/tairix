//! The HD6845S cathode-ray-tube controller: the counters that turn a
//! character clock into rows, scan lines, memory addresses and sync pulses.

/// Register numbers.
pub mod reg {
    /// Horizontal total, in characters, minus one.
    pub const H_TOTAL: usize = 0;
    /// Horizontal displayed characters.
    pub const H_DISPLAYED: usize = 1;
    /// Horizontal sync position.
    pub const H_SYNC_POS: usize = 2;
    /// Sync widths: HSYNC in bits 0–3, VSYNC lines in bits 4–7.
    pub const SYNC_WIDTH: usize = 3;
    /// Vertical total, in character rows, minus one.
    pub const V_TOTAL: usize = 4;
    /// Vertical total adjust, in scan lines.
    pub const V_ADJUST: usize = 5;
    /// Vertical displayed rows.
    pub const V_DISPLAYED: usize = 6;
    /// Vertical sync position, in rows.
    pub const V_SYNC_POS: usize = 7;
    /// Interlace mode (bits 0–1), display skew (4–5), cursor skew (6–7).
    pub const MODE: usize = 8;
    /// Scan lines per row, minus one.
    pub const MAX_RASTER: usize = 9;
    /// Cursor start line (bits 0–4) and blink mode (bits 5–6).
    pub const CURSOR_START: usize = 10;
    /// Cursor end line.
    pub const CURSOR_END: usize = 11;
    /// Display start address, high.
    pub const START_HIGH: usize = 12;
    /// Display start address, low.
    pub const START_LOW: usize = 13;
    /// Cursor address, high.
    pub const CURSOR_HIGH: usize = 14;
    /// Cursor address, low.
    pub const CURSOR_LOW: usize = 15;
    /// Light pen address, high.
    pub const PEN_HIGH: usize = 16;
    /// Light pen address, low.
    pub const PEN_LOW: usize = 17;
}

/// How many bits of each register the chip stores.
const MASKS: [u8; 18] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0x7F, 0x1F, 0x7F, 0x7F, 0xF3, 0x1F, 0x7F, 0x1F, 0x3F, 0xFF, 0x3F, 0xFF,
    0x3F, 0xFF,
];

/// The memory address counter's fourteen bits.
const ADDRESS_MASK: u16 = 0x3FFF;
/// The raster counter's five bits.
const RASTER_MASK: u8 = 0x1F;
/// The row counter's seven bits.
const ROW_MASK: u8 = 0x7F;
/// The address register's five bits.
const SELECT_MASK: u8 = 0x1F;
/// The longest HSYNC, in characters.
const LONGEST_HSYNC: u8 = 0x0F;
/// The VSYNC a width of zero programs, the longest, in lines.
const LONGEST_VSYNC: u8 = 16;

/// What the controller drives during one character.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent output of the character clock"
)]
pub struct Character {
    /// The memory address, 14 bits.
    pub address: u16,
    /// The scan line within the row, 5 bits.
    pub raster: u8,
    /// Display enable, after its skew.
    pub display: bool,
    /// The cursor, after its skew.
    pub cursor: bool,
    /// Horizontal sync.
    pub hsync: bool,
    /// Vertical sync.
    pub vsync: bool,
}

/// The chip.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent latch of the timing chain"
)]
pub struct Crtc {
    regs: [u8; 18],
    selected: u8,
    /// Horizontal character count within the line.
    hc: u8,
    /// Row count within the frame.
    vc: u8,
    /// Scan line within the row.
    ra: u8,
    /// Scan lines left in the vertical adjust, when in it.
    adjust: Option<u8>,
    /// Address of the current row's first character, and the current one.
    row_address: u16,
    address: u16,
    /// The address the next row starts at, captured at the end of display on
    /// a row's last line.
    next_row_address: u16,
    h_display: bool,
    v_display: bool,
    hsync_left: u8,
    vsync_left: u8,
    /// The current row is VSYNC's and its pulse has yet to start: on an odd
    /// interlaced field it starts half a line late.
    vsync_armed: bool,
    /// Odd field of an interlaced frame.
    odd_field: bool,
    /// Fields since power-on, for the cursor's blink.
    fields: u32,
    /// Display enable and cursor of the last three characters, for skew.
    display_pipe: u8,
    cursor_pipe: u8,
}

impl Default for Crtc {
    fn default() -> Self {
        Self::new()
    }
}

impl Crtc {
    /// A chip at power-on: registers clear, counters at the top of a frame.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            regs: [0; 18],
            selected: 0,
            hc: 0,
            vc: 0,
            ra: 0,
            adjust: None,
            row_address: 0,
            address: 0,
            next_row_address: 0,
            h_display: true,
            v_display: true,
            hsync_left: 0,
            vsync_left: 0,
            vsync_armed: false,
            odd_field: false,
            fields: 0,
            display_pipe: 0,
            cursor_pipe: 0,
        }
    }

    /// Select a register: the address register's write.
    pub fn select(&mut self, register: u8) {
        self.selected = register & SELECT_MASK;
    }

    /// Write the selected register.
    pub fn write(&mut self, value: u8) {
        if let Some(&mask) = MASKS.get(usize::from(self.selected)) {
            if usize::from(self.selected) < reg::PEN_HIGH {
                self.regs[usize::from(self.selected)] = value & mask;
            }
        }
    }

    /// Read the selected register; only the cursor and light pen registers
    /// read back.
    #[must_use]
    pub fn read(&self) -> u8 {
        match usize::from(self.selected) {
            r @ reg::CURSOR_HIGH..=reg::PEN_LOW => self.regs[r],
            _ => 0,
        }
    }

    /// A register's value.
    #[must_use]
    pub const fn register(&self, register: usize) -> u8 {
        self.regs[register]
    }

    /// Latch the address being displayed into the light pen registers.
    pub fn strobe_light_pen(&mut self) {
        let [low, high] = self.address.to_le_bytes();
        self.regs[reg::PEN_HIGH] = high & MASKS[reg::PEN_HIGH];
        self.regs[reg::PEN_LOW] = low;
    }

    /// Whether VSYNC is active.
    #[must_use]
    pub const fn vsync(&self) -> bool {
        self.vsync_left > 0
    }

    /// Whether an odd field's VSYNC waits to start half way along this line.
    #[must_use]
    pub(crate) const fn vsync_armed(&self) -> bool {
        self.vsync_armed
    }

    /// The horizontal count of the character the next clock drives.
    #[must_use]
    pub const fn next_character(&self) -> u8 {
        self.hc
    }

    /// Whether the current field is the odd one of an interlaced frame.
    #[must_use]
    pub const fn odd_field(&self) -> bool {
        self.odd_field
    }

    /// Whether this field's VSYNC starts half a line late, as an odd field's
    /// does while sync interlaces.
    #[must_use]
    pub const fn vsync_late(&self) -> bool {
        self.odd_field && self.interlaced_sync()
    }

    /// Whether the chip interlaces video as well as sync, giving each field
    /// alternate scan lines of every row.
    #[must_use]
    pub const fn interlaced_video(&self) -> bool {
        self.regs[reg::MODE] & 0x03 == 0x03
    }

    /// Whether sync alone, or sync and video, interlace.
    const fn interlaced_sync(&self) -> bool {
        self.regs[reg::MODE] & 0x01 != 0
    }

    /// The character the chip drives now, then one character clock.
    pub fn clock(&mut self) -> Character {
        let raw_display = self.h_display && self.v_display;
        let cursor_address =
            u16::from_be_bytes([self.regs[reg::CURSOR_HIGH], self.regs[reg::CURSOR_LOW]]);
        let raw_cursor = raw_display && self.address == cursor_address && self.cursor_line();
        let display = skewed(
            &mut self.display_pipe,
            raw_display,
            self.regs[reg::MODE] >> 4,
        );
        let cursor = skewed(&mut self.cursor_pipe, raw_cursor, self.regs[reg::MODE] >> 6);
        let out = Character {
            address: self.address,
            raster: self.ra,
            display,
            cursor,
            hsync: self.hsync_left > 0,
            vsync: self.vsync_left > 0,
        };
        self.advance();
        out
    }

    fn cursor_line(&self) -> bool {
        let start = self.regs[reg::CURSOR_START] & 0x1F;
        let end = self.regs[reg::CURSOR_END] & 0x1F;
        // Blinking at 1/16 or 1/32 of the field rate: on for half of a
        // 16- or 32-field period.
        let visible = match (self.regs[reg::CURSOR_START] >> 5) & 0x03 {
            0 => true,
            1 => false,
            2 => self.fields & 0x08 != 0,
            _ => self.fields & 0x10 != 0,
        };
        visible && self.ra >= start && self.ra <= end
    }

    fn advance(&mut self) {
        if self.hsync_left > 0 {
            self.hsync_left -= 1;
        }
        // Half way along the line is where an odd interlaced field's VSYNC
        // starts, so that the two fields' lines interleave.
        if self.vsync_armed && self.hc == self.regs[reg::H_TOTAL] / 2 {
            self.start_vsync();
        }
        if self.hc == self.regs[reg::H_TOTAL] {
            self.end_of_line();
            return;
        }
        self.hc = self.hc.wrapping_add(1);
        self.address = self.address.wrapping_add(1) & ADDRESS_MASK;
        if self.hc == self.regs[reg::H_DISPLAYED] {
            self.h_display = false;
            if self.last_raster() {
                self.next_row_address = self.address;
            }
        }
        if self.hc == self.regs[reg::H_SYNC_POS] {
            self.hsync_left = self.regs[reg::SYNC_WIDTH] & LONGEST_HSYNC;
        }
    }

    /// Whether the current scan line is its row's last.
    fn last_raster(&self) -> bool {
        let max = self.regs[reg::MAX_RASTER];
        if self.interlaced_video() {
            self.ra & !1 == max & !1
        } else {
            self.ra == max
        }
    }

    fn end_of_line(&mut self) {
        self.hc = 0;
        self.h_display = self.regs[reg::H_DISPLAYED] != 0;
        if self.regs[reg::H_SYNC_POS] == 0 {
            self.hsync_left = self.regs[reg::SYNC_WIDTH] & LONGEST_HSYNC;
        }
        if self.vsync_left > 0 {
            self.vsync_left -= 1;
        }
        // An odd field's pulse that was due on this line and never started
        // (its half-way point lies past a shortened line) starts now.
        if self.vsync_armed {
            self.start_vsync();
        }
        if let Some(lines) = self.adjust {
            if lines > 1 {
                self.adjust = Some(lines - 1);
                self.ra = (self.ra + 1) & RASTER_MASK;
            } else {
                self.start_frame();
                return;
            }
        } else if self.last_raster() {
            self.ra = if self.interlaced_video() {
                u8::from(self.odd_field)
            } else {
                0
            };
            if self.vc == self.regs[reg::V_TOTAL] {
                let adjust = self.regs[reg::V_ADJUST];
                if adjust == 0 {
                    self.start_frame();
                    return;
                }
                self.adjust = Some(adjust);
                self.vc = self.vc.wrapping_add(1) & ROW_MASK;
            } else {
                self.vc = self.vc.wrapping_add(1) & ROW_MASK;
            }
            self.row_address = self.next_row_address;
            self.start_row();
        } else {
            self.ra = (self.ra + if self.interlaced_video() { 2 } else { 1 }) & RASTER_MASK;
        }
        self.address = self.row_address;
    }

    fn start_frame(&mut self) {
        self.adjust = None;
        self.vc = 0;
        self.fields = self.fields.wrapping_add(1);
        self.odd_field = self.interlaced_sync() && !self.odd_field;
        self.ra = if self.interlaced_video() {
            u8::from(self.odd_field)
        } else {
            0
        };
        self.row_address =
            u16::from_be_bytes([self.regs[reg::START_HIGH], self.regs[reg::START_LOW]])
                & ADDRESS_MASK;
        self.next_row_address = self.row_address;
        self.v_display = true;
        self.start_row();
        self.address = self.row_address;
    }

    /// A new row: display stops at the displayed-row count, and VSYNC starts
    /// at its row: at once on an even field, half a line on for an odd one.
    fn start_row(&mut self) {
        if self.vc == self.regs[reg::V_DISPLAYED] {
            self.v_display = false;
        }
        if self.vc == self.regs[reg::V_SYNC_POS] && self.vsync_left == 0 && self.adjust.is_none() {
            if self.vsync_late() {
                self.vsync_armed = true;
            } else {
                self.start_vsync();
            }
        }
    }

    fn start_vsync(&mut self) {
        self.vsync_armed = false;
        let width = self.regs[reg::SYNC_WIDTH] >> 4;
        self.vsync_left = if width == 0 { LONGEST_VSYNC } else { width };
    }
}

/// Delay `now` by `skew` characters (`3`: never) through `pipe`.
fn skewed(pipe: &mut u8, now: bool, skew: u8) -> bool {
    *pipe = (*pipe << 1) | u8::from(now);
    match skew & 0x03 {
        0 => now,
        1 => *pipe & 0x02 != 0,
        2 => *pipe & 0x04 != 0,
        _ => false,
    }
}

crate::snapshot::fields!(
    Character {
        address,
        raster,
        display,
        cursor,
        hsync,
        vsync
    },
    check | character | character.address <= ADDRESS_MASK && character.raster <= RASTER_MASK
);
crate::snapshot::fields!(
    Crtc {
        regs,
        selected,
        hc,
        vc,
        ra,
        adjust,
        row_address,
        address,
        next_row_address,
        h_display,
        v_display,
        hsync_left,
        vsync_left,
        vsync_armed,
        odd_field,
        fields,
        display_pipe,
        cursor_pipe,
    },
    check | crtc | crtc.restorable()
);

impl Crtc {
    /// Whether a restored chip holds only what its registers' and counters'
    /// widths allow.
    fn restorable(&self) -> bool {
        self.regs
            .iter()
            .zip(MASKS)
            .all(|(&value, mask)| value & !mask == 0)
            && self.selected <= SELECT_MASK
            && self.vc <= ROW_MASK
            && self.ra <= RASTER_MASK
            && self
                .adjust
                .is_none_or(|lines| (1..=MASKS[reg::V_ADJUST]).contains(&lines))
            && [self.row_address, self.address, self.next_row_address]
                .iter()
                .all(|&address| address <= ADDRESS_MASK)
            && self.hsync_left <= LONGEST_HSYNC
            && self.vsync_left <= LONGEST_VSYNC
    }
}
