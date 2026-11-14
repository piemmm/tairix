//! The Electron's picture: the ULA's display of modes 0–6, each byte fetched
//! from RAM at the cycle the beam reaches it and shown through the palette,
//! painted onto the canvas; and how long those fetches hold the CPU off RAM.
//!
//! A frame is 312 lines of 128 cycles, not interlaced; a line's cycles count
//! from its HSYNC and its 640 pixels take cycles 24 to 104. The first 256
//! lines carry pixels. There is no cursor hardware and no border colour.

use crate::machine::canvas::{encode, Canvas, DirtyRows, BLACK, CANVAS_LINES};
use crate::machine::SIDEWAYS_BASE;

/// Cycles in a line, from its HSYNC.
pub const LINE_CYCLES: u64 = 128;
/// Lines in a frame.
pub const FRAME_LINES: u64 = 312;
/// Cycles in a frame.
pub const FRAME_CYCLES: u64 = LINE_CYCLES * FRAME_LINES;
/// Lines that carry pixels.
pub const DISPLAY_LINES: u64 = 256;

/// The line cycles the display window spans: where the ULA owns RAM in modes
/// 0–3.
const WINDOW: core::ops::Range<u64> = 24..104;
/// The line cycle at the canvas's left edge.
const LEFT_EDGE: u64 = 8;
/// Canvas lines above the first display line, which centre the display.
const TOP_EDGE: u64 = (CANVAS_LINES as u64 - DISPLAY_LINES) / 2;

/// Slots of the canvas a cycle paints: the 16 MHz pixel clock's eight.
const SLOTS_PER_CYCLE: usize = 8;

/// A mode's shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shape {
    /// Bytes in a row of characters: eight lines of its bytes.
    row_bytes: u64,
    /// Where its screen starts unscrolled, and where an address past the top
    /// of RAM wraps to.
    base: u16,
    /// Bits a pixel takes.
    bits: u8,
    /// RAM is read at 1 MHz, a byte each two cycles of the window.
    slow: bool,
    /// Rows of ten lines, eight of them pixels, 25 of them.
    text: bool,
}

/// The shape of mode `mode`, 7 being taken as 4.
const fn shape(mode: u8) -> Shape {
    let (row_bytes, base, bits, slow, text) = match mode {
        0 => (640, 0x3000, 1, false, false),
        1 => (640, 0x3000, 2, false, false),
        2 => (640, 0x3000, 4, false, false),
        3 => (640, 0x4000, 1, false, true),
        5 => (320, 0x5800, 2, true, false),
        6 => (320, 0x6000, 1, true, true),
        _ => (320, 0x5800, 1, true, false),
    };
    Shape {
        row_bytes,
        base,
        bits,
        slow,
        text,
    }
}

/// Rows a text mode shows, and the lines each row takes.
const TEXT_ROWS: u64 = 25;
const TEXT_ROW_LINES: u64 = 10;

/// The four palette entries each register pair holds, `&FE08` and `&FE09`
/// the first pair.
pub const SETS: [[u8; 4]; 4] = [[0, 2, 8, 10], [4, 6, 12, 14], [5, 7, 13, 15], [1, 3, 9, 11]];

/// Where a register pair holds the guns of each of its four entries: the
/// even register's bit for blue; the bit for green, and whether it is the
/// even register's; and the odd register's bit for red. A bit set turns its
/// gun off.
pub const GUNS: [(u8, (bool, u8), u8); 4] = [
    (0x10, (false, 0x10), 0x01),
    (0x20, (false, 0x20), 0x02),
    (0x40, (true, 0x04), 0x04),
    (0x80, (true, 0x08), 0x08),
];

/// The bits of the even and odd registers that give the entry at `place` in
/// its pair `colour`, a bit each of red, green and blue.
#[must_use]
pub const fn register_bits(place: usize, colour: u8) -> (u8, u8) {
    let (blue, (green_even, green), red) = GUNS[place];
    let mut even = if colour & 4 == 0 { blue } else { 0 };
    let mut odd = if colour & 1 == 0 { red } else { 0 };
    if colour & 2 == 0 {
        if green_even {
            even |= green;
        } else {
            odd |= green;
        }
    }
    (even, odd)
}

/// The colours a register pair gives its four entries.
fn colours_of(even: u8, odd: u8) -> [u8; 4] {
    core::array::from_fn(|place| {
        let (blue, (green_even, green), red) = GUNS[place];
        let on = |register: u8, bit: u8| u8::from(register & bit == 0);
        let green = on(if green_even { even } else { odd }, green);
        on(odd, red) | (green << 1) | (on(even, blue) << 2)
    })
}

/// The palette entry pixel `pixel` of `byte` shows at `bits` a pixel: its
/// colour's bits in place, so a two-colour pixel is entry 0 or 8, a
/// four-colour one 0, 2, 8 or 10.
fn entry(byte: u8, pixel: u64, bits: u8) -> usize {
    let bit = |at: u64| usize::from((byte >> at) & 1);
    match bits {
        1 => bit(7 - pixel) << 3,
        2 => (bit(7 - pixel) << 3) | (bit(3 - pixel) << 1),
        _ => (bit(7 - pixel) << 3) | (bit(5 - pixel) << 2) | (bit(3 - pixel) << 1) | bit(1 - pixel),
    }
}

/// The ULA's display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Video {
    /// The first cycle not yet drawn.
    cycle: u64,
    /// The mode the line being drawn shows, and the one last written, which
    /// the next line takes.
    mode: u8,
    written_mode: u8,
    /// The screen start written, and the one the frame took at its start, in
    /// bytes; zero is the mode's base.
    start_written: u16,
    start: u16,
    palette: [u8; 8],
    /// Each palette entry's slot, from `palette`.
    slots: [u8; 16],
    /// The byte a slow mode fetched for the two cycles it shows.
    latched: u8,
    canvas: Canvas,
}

impl Default for Video {
    fn default() -> Self {
        Self::new()
    }
}

impl Video {
    /// The display at power-on: mode 0, every palette entry white.
    #[must_use]
    pub fn new() -> Self {
        let mut video = Self {
            cycle: 0,
            mode: 0,
            written_mode: 0,
            start_written: 0,
            start: 0,
            palette: [0; 8],
            slots: [BLACK; 16],
            latched: 0,
            canvas: Canvas::new(),
        };
        video.derive_slots();
        video
    }

    fn derive_slots(&mut self) {
        for (pair, set) in SETS.iter().enumerate() {
            let colours = colours_of(self.palette[2 * pair], self.palette[2 * pair + 1]);
            for (&entry, colour) in set.iter().zip(colours) {
                self.slots[usize::from(entry)] = encode(colour, 0, 3);
            }
        }
    }

    /// Frames finished since power-on.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.canvas.frames()
    }

    /// The last finished frame.
    #[must_use]
    pub fn shown(&self) -> &[u8] {
        self.canvas.shown()
    }

    /// The rows of the finished frame that changed since last asked.
    pub fn take_dirty(&mut self) -> DirtyRows {
        self.canvas.take_dirty()
    }

    /// The mode the line holding cycle `now` shows, the beam drawn up to it
    /// or not: a line started since the beam last moved takes the mode last
    /// written.
    #[must_use]
    pub const fn mode_at(&self, now: u64) -> u8 {
        if self.cycle == 0 || now / LINE_CYCLES > (self.cycle - 1) / LINE_CYCLES {
            self.written_mode
        } else {
            self.mode
        }
    }

    /// Cycles a RAM access beginning at `now` waits before it is made: in
    /// modes 4–6 to the next coincident edge of the 1 MHz clock, as the
    /// Model B's 1 MHz bus does; in modes 0–3, while the display window of a
    /// display line holds RAM, until the window ends.
    #[must_use]
    pub const fn ram_wait(&self, now: u64) -> u64 {
        if shape(self.mode_at(now)).slow {
            return crate::machine::one_mhz_stretch(now);
        }
        let in_frame = now % FRAME_CYCLES;
        let (line, cycle) = (in_frame / LINE_CYCLES, in_frame % LINE_CYCLES);
        if line < DISPLAY_LINES && cycle >= WINDOW.start && cycle < WINDOW.end {
            WINDOW.end - cycle
        } else {
            0
        }
    }

    /// The CPU writes `&FE07`'s mode bits: the next line shows it.
    pub const fn write_mode(&mut self, mode: u8) {
        self.written_mode = mode & 7;
    }

    /// The CPU writes the screen start's `&FE02` (`high` false) or `&FE03`
    /// register: the next frame takes it.
    pub fn write_start(&mut self, high: bool, value: u8) {
        self.start_written = if high {
            (self.start_written & 0x01C0) | (u16::from(value & 0x3F) << 9)
        } else {
            (self.start_written & 0x7E00) | (u16::from(value & 0xE0) << 1)
        };
    }

    /// The CPU writes palette register `register` (`&FE08` + `register`).
    pub fn write_palette(&mut self, register: usize, value: u8) {
        if let Some(held) = self.palette.get_mut(register) {
            *held = value;
            self.derive_slots();
        }
    }

    /// Draw every cycle before `until` from `ram`.
    pub fn catch_up(&mut self, until: u64, ram: &[u8]) {
        while self.cycle < until {
            let in_frame = self.cycle % FRAME_CYCLES;
            let (line, cycle) = (in_frame / LINE_CYCLES, in_frame % LINE_CYCLES);
            if cycle == 0 {
                self.start_line(line);
            }
            let line_start = self.cycle - cycle;
            let to = until.min(line_start + LINE_CYCLES);
            if line < DISPLAY_LINES {
                let first = cycle.max(WINDOW.start);
                let last = (to - line_start).min(WINDOW.end);
                for at in first..last {
                    self.draw(line, at, ram);
                }
            }
            self.cycle = to;
        }
    }

    /// A line's HSYNC: it takes the mode last written; the frame takes its
    /// screen start at its first line, and is finished once its last display
    /// line is drawn.
    fn start_line(&mut self, line: u64) {
        self.mode = self.written_mode;
        if line == 0 {
            self.start = self.start_written;
        } else if line == DISPLAY_LINES {
            self.canvas.finish();
        }
    }

    /// Paint display line `line`'s cycle `at`, within the window.
    fn draw(&mut self, line: u64, at: u64, ram: &[u8]) {
        let shape = shape(self.mode);
        let window = at - WINDOW.start;
        let (byte, half) = if shape.slow {
            (window / 2, window % 2)
        } else {
            (window, 0)
        };
        let mut slots = [BLACK; SLOTS_PER_CYCLE];
        if let Some(offset) = Self::line_offset(shape, line) {
            if half == 0 {
                self.latched = ram[self.address(shape, offset + byte * 8)];
            }
            let pixels = u64::from(8 / shape.bits);
            let per_pixel = (if shape.slow { 16 } else { 8 }) / pixels;
            for (n, slot) in (0..).zip(slots.iter_mut()) {
                let pixel = (half * 8 + n) / per_pixel;
                *slot = self.slots[entry(self.latched, pixel, shape.bits)];
            }
        }
        let (row, column) = (2 * (line + TOP_EDGE), (at - LEFT_EDGE) * 8);
        if let (Ok(row), Ok(column)) = (usize::try_from(row), usize::try_from(column)) {
            self.canvas.paint(row, column, &slots);
            self.canvas.paint(row + 1, column, &slots);
        }
    }

    /// Bytes into the screen display line `line` starts at, none for a text
    /// mode's blank lines.
    const fn line_offset(shape: Shape, line: u64) -> Option<u64> {
        let (row, scan) = if shape.text {
            (line / TEXT_ROW_LINES, line % TEXT_ROW_LINES)
        } else {
            (line / 8, line % 8)
        };
        if shape.text && (row >= TEXT_ROWS || scan >= 8) {
            return None;
        }
        Some(row * shape.row_bytes + scan)
    }

    /// The RAM address `offset` bytes into the screen: from the frame's
    /// start, the mode's base for zero, wrapping past the top of RAM to the
    /// mode's base.
    fn address(&self, shape: Shape, offset: u64) -> usize {
        let start = if self.start == 0 {
            shape.base
        } else {
            self.start
        };
        let mut address = u64::from(start) + offset;
        let top = u64::from(SIDEWAYS_BASE);
        if address >= top {
            address = address - top + u64::from(shape.base);
        }
        usize::try_from(address % top).unwrap_or(0)
    }
}

impl crate::snapshot::Save for Video {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        w.put(&self.cycle);
        w.put(&self.mode);
        w.put(&self.written_mode);
        w.put(&self.start_written);
        w.put(&self.start);
        w.put(&self.palette);
        w.put(&self.latched);
        w.put(&self.canvas);
    }
}

impl crate::snapshot::Load for Video {
    /// A restored display is drawn up to the machine's cycle, as a snapshot
    /// leaves it, its modes and starts ones the registers can hold.
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        let mut video = Self {
            cycle: r.get()?,
            mode: r.get()?,
            written_mode: r.get()?,
            start_written: r.get()?,
            start: r.get()?,
            palette: r.get()?,
            slots: [BLACK; 16],
            latched: r.get()?,
            canvas: r.get()?,
        };
        let start = |start: u16| start & !0x7FC0 == 0;
        crate::snapshot::Reader::check(
            (r.clock()..=r.clock() + 1).contains(&video.cycle)
                && video.mode <= 7
                && video.written_mode <= 7
                && start(video.start_written)
                && start(video.start),
        )?;
        video.derive_slots();
        Ok(video)
    }
}

#[cfg(test)]
#[path = "video_tests.rs"]
mod tests;
