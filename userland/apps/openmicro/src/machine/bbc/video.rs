//! The picture: the 6845, the video ULA and the SAA5050 driven cycle by cycle,
//! and the beam that paints their output onto a canvas locked to sync as a
//! television locks to it.
//!
//! A program that moves HSYNC or VSYNC moves the picture, one that changes the
//! palette in the middle of a line changes it there, and one that writes the
//! screen just behind the beam is seen to.

use super::crtc::{reg, Character, Crtc};
use super::teletext::{self, Saa5050};
use super::ula::{Ula, SLOTS_PER_CYCLE};
use crate::machine::canvas::{
    encode, Canvas, DirtyRows, BLACK, CANVAS_HEIGHT, CANVAS_LINES, CANVAS_WIDTH,
};

const _: () = assert!(CANVAS_WIDTH.is_multiple_of(SLOTS_PER_CYCLE));

/// Cycles from the start of HSYNC to the canvas's left edge: 8 µs. The beam
/// moves on by one cycle's slots each cycle.
const LEFT_EDGE: i32 = 16;

/// Lines from the start of VSYNC to the canvas's top edge.
const TOP_EDGE: i32 = 16;

/// Cycles in a line of a television's free-running timebase: 64 µs.
const LINE_CYCLES: i32 = 128;

/// How long after a line's sync the television waits for the next before its
/// own timebase starts one: a quarter of a line beyond where it is due.
const LINE_TIMEOUT: i32 = LINE_CYCLES + LINE_CYCLES / 4;

/// Lines a television's field timebase runs before it fires without VSYNC.
const FIELD_LINES: i32 = 320;

/// Characters the SAA5050's dots trail the byte it was given, a character
/// after its fetch: three in all, under the cursor the 6845 and ULA skew.
const TELETEXT_DELAY: usize = 2;

/// Cycles from the beam passing under the light pen to its strobe: 3 µs. The
/// latched address is then the Advanced User Guide's correction ahead of the
/// pen's character in modes 4–6, and within two characters of it otherwise.
const PEN_RESPONSE: u64 = 6;
/// Cycles the light pen holds its strobe high.
const PEN_PULSE: u64 = 4;

/// A line the video system drives into the System VIA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// VSYNC, into CA1.
    Vsync(bool),
    /// The light pen's strobe, into CB2.
    LightPen(bool),
}

/// The video system.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent line from the CRTC or field state"
)]
pub struct Video {
    crtc: Crtc,
    ula: Ula,
    teletext: Saa5050,
    /// The first cycle not yet drawn.
    cycle: u64,
    /// The character being drawn, which spans two cycles at 1 MHz.
    current: Character,
    byte: u8,
    /// The byte fetched for the previous character, which the SAA5050 is
    /// given now.
    latched: u8,
    cursor: bool,
    /// The SAA5050's output for the last few characters, newest last.
    teletext_out: [[u8; teletext::SLOTS]; TELETEXT_DELAY + 1],
    hsync: bool,
    vsync: bool,
    display: bool,
    /// Beam position: slots from the canvas's left edge, lines from its top.
    x: i32,
    y: i32,
    /// The field being drawn is odd, and its lines interleave with the
    /// other field's rather than repeating them.
    odd_field: bool,
    weave: bool,
    /// The field being drawn, and the last one finished.
    canvas: Canvas,
    /// The canvas slot and row the light pen is held over.
    pen: Option<(usize, usize)>,
    /// The cycle the pen's strobe rises, and the one it falls.
    pen_rise: Option<u64>,
    pen_fall: Option<u64>,
}

impl Default for Video {
    fn default() -> Self {
        Self::new()
    }
}

impl Video {
    /// The video system at power-on, with a black picture.
    #[must_use]
    pub fn new() -> Self {
        Self {
            crtc: Crtc::new(),
            ula: Ula::new(),
            teletext: Saa5050::new(),
            cycle: 0,
            current: Character::default(),
            byte: 0,
            latched: 0,
            cursor: false,
            teletext_out: [[BLACK; teletext::SLOTS]; TELETEXT_DELAY + 1],
            hsync: false,
            vsync: false,
            display: false,
            x: 0,
            y: 0,
            odd_field: false,
            weave: false,
            canvas: Canvas::new(),
            pen: None,
            pen_rise: None,
            pen_fall: None,
        }
    }

    /// The 6845.
    #[must_use]
    pub const fn crtc(&self) -> &Crtc {
        &self.crtc
    }

    /// Whether VSYNC is active, as the System VIA's CA1 sees it.
    #[must_use]
    pub const fn vsync(&self) -> bool {
        self.vsync
    }

    /// Fields finished since power-on.
    #[must_use]
    pub const fn fields(&self) -> u64 {
        self.canvas.frames()
    }

    /// The last finished field, `CANVAS_WIDTH` slots to a row.
    #[must_use]
    pub fn shown(&self) -> &[u8] {
        self.canvas.shown()
    }

    /// The rows of the finished field that changed since this was last
    /// called.
    pub fn take_dirty(&mut self) -> DirtyRows {
        self.canvas.take_dirty()
    }

    /// The CPU writes the 6845's address (`data` false) or data register.
    pub fn write_crtc(&mut self, data: bool, value: u8) {
        if data {
            self.crtc.write(value);
        } else {
            self.crtc.select(value);
        }
    }

    /// The CPU reads the 6845's data register.
    #[must_use]
    pub fn read_crtc(&self) -> u8 {
        self.crtc.read()
    }

    /// The CPU writes the ULA's control (`palette` false) or palette register.
    pub fn write_ula(&mut self, palette: bool, value: u8) {
        if palette {
            self.ula.write_palette(value);
        } else {
            self.ula.write_control(value);
        }
    }

    /// Hold the light pen over canvas slot `column` of row `row`, or take it
    /// away: it strobes when the beam lights the slot beneath it on either
    /// field's line there, as a pen far wider than a scan line sees both.
    pub fn set_light_pen(&mut self, at: Option<(usize, usize)>) {
        self.pen = at;
    }

    /// The cycle at which VSYNC or the light pen's strobe can next change:
    /// the 6845 changes VSYNC only at the start of a line, or half way along
    /// one for an odd field, and the pen strobes once the beam lights the
    /// slot beneath it.
    #[must_use]
    pub fn next_event(&self) -> u64 {
        let sync = self.next_sync_change();
        [self.pen_rise, self.pen_fall, self.pen_due()]
            .into_iter()
            .flatten()
            .fold(sync, u64::min)
    }

    /// The earliest cycle the beam can paint the slot under the held pen
    /// while its strobe is idle: exactly when the beam reaches its column on
    /// the pen's line, and on the line before, no sooner than a sync at once
    /// would bring it there. Waking then, the machine looks again.
    fn pen_due(&self) -> Option<u64> {
        let idle = self.pen_rise.is_none() && self.pen_fall.is_none();
        let (column, row) = self.pen.filter(|_| idle)?;
        let line = i32::try_from(row / 2).ok()?;
        let x = i32::try_from(column / SLOTS_PER_CYCLE).ok()?;
        let ahead = if self.y == line && self.x <= x {
            x - self.x
        } else if self.y + 1 == line {
            LEFT_EDGE + x
        } else {
            return None;
        };
        Some(self.cycle + u64::try_from(ahead).ok()?)
    }

    fn next_sync_change(&self) -> u64 {
        let total = u64::from(self.crtc.register(reg::H_TOTAL));
        let next = u64::from(self.crtc.next_character());
        // The horizontal count wraps at 256 when the total is set below it,
        // and a change shows on the character after the one that makes it.
        let to_line_start = (total + 256 - next) % 256 + 1;
        let characters = if self.crtc.vsync_armed() {
            to_line_start.min((total / 2 + 256 - next) % 256 + 1)
        } else {
            to_line_start
        };
        if self.ula.fast_clock() {
            self.cycle + characters
        } else {
            let boundary = self.cycle + (self.cycle & 1);
            boundary + 2 * characters
        }
    }

    /// Draw every cycle before `until`, reading screen memory from `ram` and
    /// wrapping hardware-scrolled addresses by `wrap` (the System VIA latch's
    /// screen size); report each change of a line into the System VIA, with
    /// the cycle it happened in, to `on_signal`.
    pub fn catch_up(
        &mut self,
        until: u64,
        ram: &[u8],
        wrap: u16,
        mut on_signal: impl FnMut(u64, Signal),
    ) {
        while self.cycle < until {
            self.light_pen(&mut on_signal);
            let fast = self.ula.fast_clock();
            let second_half = !fast && self.cycle & 1 == 1;
            if !second_half {
                self.next_character(ram, wrap, &mut on_signal);
            }
            self.draw(second_half);
            self.cycle += 1;
        }
    }

    /// The pen's strobe rises, latching the 6845's address, and falls.
    fn light_pen(&mut self, on_signal: &mut impl FnMut(u64, Signal)) {
        if self.pen_rise == Some(self.cycle) {
            self.pen_rise = None;
            self.pen_fall = Some(self.cycle + PEN_PULSE);
            self.crtc.strobe_light_pen();
            on_signal(self.cycle, Signal::LightPen(true));
        } else if self.pen_fall == Some(self.cycle) {
            self.pen_fall = None;
            on_signal(self.cycle, Signal::LightPen(false));
        }
    }

    fn next_character(&mut self, ram: &[u8], wrap: u16, on_signal: &mut impl FnMut(u64, Signal)) {
        let character = self.crtc.clock();
        if character.hsync && !self.hsync {
            self.horizontal_sync();
        }
        self.hsync = character.hsync;
        if character.vsync != self.vsync {
            self.vsync = character.vsync;
            on_signal(self.cycle, Signal::Vsync(self.vsync));
            self.teletext.set_dew(self.vsync);
            if self.vsync {
                self.vertical_sync();
            }
        }
        if character.display && !self.display {
            self.teletext.start_line();
        }
        self.display = character.display;
        self.latched = self.byte;
        self.byte = ram[usize::from(screen_address(character.address, character.raster, wrap))];
        if character.cursor {
            self.ula.start_cursor();
        }
        self.cursor = self.ula.take_cursor();
        if self.ula.teletext() {
            self.teletext_out.rotate_left(1);
            let newest = &mut self.teletext_out[TELETEXT_DELAY];
            if character.display {
                self.teletext
                    .character(self.latched, character.raster & 1 != 0, newest);
            } else {
                newest.fill(BLACK);
            }
        }
        self.current = character;
    }

    /// A line's sync: the beam flies back, and the SAA5050 counts the line.
    fn horizontal_sync(&mut self) {
        self.x = -LEFT_EDGE;
        self.y += 1;
        self.teletext.end_line();
    }

    /// A field's sync: the beam returns to the top, and the field drawn is
    /// finished.
    fn vertical_sync(&mut self) {
        self.canvas.finish();
        self.y = -TOP_EDGE;
        self.odd_field = self.crtc.odd_field();
        self.weave = self.crtc.interlaced_video();
    }

    /// Paint this cycle's eight slots at the beam and move it on.
    fn draw(&mut self, second_half: bool) {
        let mut slots = [BLACK; SLOTS_PER_CYCLE];
        if self.ula.teletext() {
            // Already blank where the display was disabled when drawn.
            let start = if second_half { SLOTS_PER_CYCLE } else { 0 };
            slots.copy_from_slice(&self.teletext_out[0][start..start + SLOTS_PER_CYCLE]);
        } else if self.current.display {
            let shifted = if second_half {
                SLOTS_PER_CYCLE / self.ula.slots_per_shift()
            } else {
                0
            };
            self.ula.serialise(self.byte, shifted, &mut slots);
            for slot in &mut slots {
                *slot = encode(self.ula.colour(*slot), 0, 3);
            }
        }
        if self.cursor {
            for slot in &mut slots {
                *slot ^= 0x3F;
            }
        }
        self.paint(slots);
        self.x += 1;
        if self.x >= LINE_TIMEOUT - LEFT_EDGE {
            // No HSYNC within a line's time: the television's timebase
            // starts the next line on its own.
            self.x -= LINE_CYCLES;
            self.y += 1;
        }
        if self.y >= FIELD_LINES {
            self.canvas.finish();
            self.y = -TOP_EDGE;
        }
    }

    fn paint(&mut self, slots: [u8; SLOTS_PER_CYCLE]) {
        let (Ok(line), Ok(column)) = (usize::try_from(self.y), usize::try_from(self.x)) else {
            return;
        };
        let first = column * SLOTS_PER_CYCLE;
        if line >= CANVAS_LINES || first >= CANVAS_WIDTH {
            return;
        }
        // An interlaced field draws its own rows; otherwise each line fills
        // both rows of its pair.
        let (rows, count) = if self.weave {
            ([2 * line + usize::from(self.odd_field), 0], 1)
        } else {
            ([2 * line, 2 * line + 1], 2)
        };
        for &row in &rows[..count] {
            self.canvas.paint(row, first, &slots);
        }
        let idle = self.pen_rise.is_none() && self.pen_fall.is_none();
        if let Some((column, row)) = self.pen.filter(|_| idle) {
            let under = column
                .checked_sub(first)
                .filter(|&offset| offset < SLOTS_PER_CYCLE);
            if row / 2 == line && under.is_some_and(|offset| lit(slots[offset])) {
                self.pen_rise = Some(self.cycle + PEN_RESPONSE);
            }
        }
    }
}

impl crate::snapshot::Save for Video {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        w.put(&self.crtc);
        w.put(&self.ula);
        w.put(&self.teletext);
        w.put(&self.cycle);
        w.put(&self.current);
        w.put(&self.byte);
        w.put(&self.latched);
        w.put(&self.cursor);
        w.put(&self.teletext_out);
        w.put(&self.x);
        w.put(&self.y);
        w.put(&self.odd_field);
        w.put(&self.weave);
        w.put(&self.canvas);
        w.put(&self.pen);
        w.put(&self.pen_rise);
        w.put(&self.pen_fall);
    }
}

impl crate::snapshot::Load for Video {
    /// The sync and display lines are the current character's.
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        let crtc = r.get()?;
        let ula = r.get()?;
        let teletext = r.get()?;
        let cycle = r.get()?;
        let current: Character = r.get()?;
        let video = Self {
            crtc,
            ula,
            teletext,
            cycle,
            current,
            byte: r.get()?,
            latched: r.get()?,
            cursor: r.get()?,
            teletext_out: r.get()?,
            hsync: current.hsync,
            vsync: current.vsync,
            display: current.display,
            x: r.get()?,
            y: r.get()?,
            odd_field: r.get()?,
            weave: r.get()?,
            canvas: r.get()?,
            pen: r.get()?,
            pen_rise: r.get()?,
            pen_fall: r.get()?,
        };
        crate::snapshot::Reader::check(video.restorable(r))?;
        Ok(video)
    }
}

impl Video {
    /// Whether a restored picture is one the beam can be drawing: drawn up to
    /// the machine's cycle, as a snapshot leaves it, and at most the one after
    /// it; the beam on the canvas's timebase; and the pen's strobe, if under
    /// way, due within its timing.
    fn restorable(&self, r: &crate::snapshot::Reader<'_>) -> bool {
        let within = |at: Option<u64>, by: u64| {
            at.is_none_or(|at| at >= self.cycle && at <= self.cycle + by)
        };
        (r.clock()..=r.clock() + 1).contains(&self.cycle)
            && (-LEFT_EDGE..LINE_TIMEOUT - LEFT_EDGE).contains(&self.x)
            && (-TOP_EDGE..FIELD_LINES).contains(&self.y)
            && self
                .pen
                .is_none_or(|(column, row)| column < CANVAS_WIDTH && row < CANVAS_HEIGHT)
            && (self.pen_rise.is_none() || self.pen_fall.is_none())
            && within(self.pen_rise, PEN_RESPONSE)
            && within(self.pen_fall, PEN_PULSE)
    }
}

/// Whether a slot gives out light: some colour other than black shows.
const fn lit(slot: u8) -> bool {
    let (foreground, background, coverage) = (slot & 0x07, (slot >> 3) & 0x07, slot >> 6);
    (coverage > 0 && foreground != 0) || (coverage < 3 && background != 0)
}

/// Where the 6845's address and scan line read screen memory from.
///
/// With address bit 13 set the 6845 is reading teletext memory: ten address
/// bits, placed at `&3C00` or `&7C00` by bit 11. Otherwise each address names
/// eight bytes, one per scan line, and an address past the top of memory has
/// the screen size `wrap` added to bring it back to the start of the screen.
#[must_use]
pub fn screen_address(address: u16, raster: u8, wrap: u16) -> u16 {
    if address & 0x2000 != 0 {
        return ((address & 0x0800) << 3) | 0x3C00 | (address & 0x03FF);
    }
    let linear = ((address & 0x1FFF) << 3) | (u16::from(raster) & 0x07);
    if linear & 0x8000 != 0 {
        linear.wrapping_add(wrap) & 0x7FFF
    } else {
        linear
    }
}

#[cfg(test)]
#[path = "video_tests.rs"]
mod tests;
