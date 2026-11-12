//! The IBM Enhanced Graphics Adapter and Video Graphics Array.
//!
//! Both are four planes of memory behind a sequencer and graphics
//! controller, scanned out by their CRT controller through the attribute
//! controller's palette; the VGA adds a DAC, 256-colour modes, chain-4
//! addressing and readable registers. A frame's visible window is where the
//! CRT controller's blanking leaves the beam lit, so every mode and border
//! the timing programs shows as programmed.

pub mod crtc;
pub mod planes;

use self::crtc::{Crtc, Generation, Tick};
use self::planes::{Planes, Routing};
use super::font::joins_ninth_column;
use super::monitor::{Monitor, CGA_COLOURS};
use super::raster::{Conversion, Palette, Raster, RasterError, Shades, Window};
use super::{read_text, Frame, ShownText, Unread};
use crate::time::{Clock, Hertz, Nanos};

/// The attribute controller's mode bits.
mod ac_mode {
    pub const GRAPHICS: u8 = 0x01;
    pub const LINE_GRAPHICS: u8 = 0x04;
    pub const BLINK: u8 = 0x08;
    pub const PANNING_RESET: u8 = 0x20;
    pub const EIGHT_BIT: u8 = 0x40;
    pub const P54: u8 = 0x80;
}

/// The graphics controller's mode bits that shape the display.
mod gc_display {
    pub const INTERLEAVE: u8 = 0x20;
    pub const COLOUR_256: u8 = 0x40;
}

/// What a register write changed that the board must act on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Moved {
    /// The memory window.
    pub window: bool,
    /// The CRT controller's and status ports, between mono and colour.
    pub ports: bool,
}

/// Ports the card decodes, colour or mono.
const COLOUR_PORTS: &[(u16, u16)] = &[(0x3C0, 0x3CF), (0x3D4, 0x3D5), (0x3DA, 0x3DA)];
const MONO_PORTS: &[(u16, u16)] = &[(0x3B4, 0x3B5), (0x3BA, 0x3BA), (0x3C0, 0x3CF)];

/// The window before the first frame programs one.
const POWER_UP: Window = Window {
    left: 0,
    width: 640,
    top: 0,
    height: 400,
    hold_line: 512,
    free_line: 2048,
    hold_frame: 256,
    free_frame: 1024,
};

/// The EGA's configuration switches as its BIOS reads them, one a time
/// through input status 0: an Enhanced Color Display in high resolution.
const EGA_SWITCHES: u8 = 0b1001;

/// Each plane's 64 KiB.
const PLANE_BYTES: usize = 0x1_0000;

/// The misc output's bit that connects the memory to the CPU.
const RAM_ENABLE: u8 = 0x02;

/// The ports a disabled VGA still answers: its enable register.
const ENABLE_PORT: &[(u16, u16)] = &[(0x3C3, 0x3C3)];

/// The card.
#[derive(Clone, Debug)]
pub struct Vga {
    generation: Generation,
    crtc: Crtc,
    clock: Clock,
    done: u64,
    planes: Planes,
    seq_index: u8,
    seq: [u8; 5],
    gc_index: u8,
    gc: [u8; 9],
    ac_index: u8,
    ac_data_next: bool,
    ac: [u8; 0x15],
    misc: u8,
    /// The VGA's enable register: off, the card answers nothing else.
    enabled: bool,
    feature: u8,
    dac: [[u8; 3]; 256],
    dac_write: u8,
    dac_read: u8,
    dac_component: u8,
    dac_reading: bool,
    dac_mask: u8,
    monitor: Monitor,
    raster: Raster,
    last: Tick,
    panning: Panning,
}

/// Pixel panning's progress through a line.
#[derive(Clone, Copy, Debug, Default)]
struct Panning {
    /// Dots still to drop from this line's display.
    skip: u32,
    /// Whether the last clock displayed, to finish a panned line.
    displaying: bool,
    /// Panning is suspended below a split.
    suspended: bool,
}

impl Vga {
    /// A card of `generation` with 256 KiB on `monitor`, as power-up leaves
    /// it.
    pub fn new(generation: Generation, monitor: Monitor) -> Result<Self, RasterError> {
        let planes = Planes::new(PLANE_BYTES).map_err(|_| RasterError::OutOfMemory)?;
        let last = Tick {
            addr: 0,
            row: 0,
            display: false,
            blank: true,
            cursor: false,
            hsync: false,
            vsync: false,
            split: false,
        };
        let mut card = Self {
            generation,
            crtc: Crtc::new(generation),
            clock: Clock::new(Hertz::CRYSTAL.divided(8).unwrap_or(Hertz::CRYSTAL)),
            done: 0,
            planes,
            seq_index: 0,
            seq: [0; 5],
            gc_index: 0,
            gc: [0; 9],
            ac_index: 0x20,
            ac_data_next: false,
            ac: [0; 0x15],
            misc: 0,
            enabled: true,
            feature: 0,
            dac: [[0; 3]; 256],
            dac_write: 0,
            dac_read: 0,
            dac_component: 0,
            dac_reading: false,
            dac_mask: 0xFF,
            monitor,
            raster: Raster::new(POWER_UP, Conversion::Rgb)?,
            last,
            panning: Panning::default(),
        };
        card.retime();
        Ok(card)
    }

    fn vga(&self) -> bool {
        self.generation == Generation::Vga
    }

    /// The dot clock the misc output selects.
    fn dot_clock(&self) -> Hertz {
        match (self.generation, (self.misc >> 2) & 3) {
            (Generation::Ega, 0) => Hertz::CRYSTAL,
            (Generation::Ega, _) => Hertz::MONO_DOTS,
            (Generation::Vga, 0) => Hertz::VGA_25,
            (Generation::Vga, _) => Hertz::VGA_28,
        }
    }

    /// Dots a character clock lasts in the raster: eight or nine, doubled
    /// when the sequencer halves the dot clock.
    fn dots_per_clock(&self) -> u32 {
        let cell = if self.seq[1] & 0x01 != 0 { 8 } else { 9 };
        if self.seq[1] & 0x08 != 0 {
            cell * 2
        } else {
            cell
        }
    }

    /// Restarts the character clock at the rate the registers now ask.
    fn retime(&mut self) {
        let rate = self
            .dot_clock()
            .divided(u64::from(self.dots_per_clock()))
            .unwrap_or(Hertz::CRYSTAL);
        if rate != self.clock.rate() {
            self.clock.set_rate(rate, self.done);
        }
    }

    /// The I/O ranges the card decodes now.
    #[must_use]
    pub fn ports(&self) -> &'static [(u16, u16)] {
        if !self.enabled {
            ENABLE_PORT
        } else if self.misc & 0x01 != 0 {
            COLOUR_PORTS
        } else {
            MONO_PORTS
        }
    }

    /// The memory the card answers: its base and length, none while the
    /// misc output or the enable register disconnects it.
    #[must_use]
    pub fn window(&self) -> (u32, u32) {
        let (base, len) = match (self.gc[6] >> 2) & 3 {
            0 => (0xA_0000, 0x2_0000),
            1 => (0xA_0000, 0x1_0000),
            2 => (0xB_0000, 0x8000),
            _ => (0xB_8000, 0x8000),
        };
        let connected = self.enabled && self.misc & RAM_ENABLE != 0;
        (base, if connected { len } else { 0 })
    }

    fn routing(&self) -> Routing {
        Routing {
            map_mask: self.seq[2],
            memory_mode: self.seq[4],
            gc: self.gc,
        }
    }

    fn offset(&self, addr: u32) -> u32 {
        addr.wrapping_sub(self.window().0)
    }

    /// Whether the vertical interrupt is asking for IRQ 2.
    #[must_use]
    pub fn irq(&self) -> bool {
        self.crtc.interrupt()
    }

    /// When the vertical interrupt next latches, if it is enabled.
    #[must_use]
    pub fn next_interrupt(&self) -> Option<Nanos> {
        self.crtc
            .clocks_to_interrupt()
            .map(|clocks| self.clock.time_of(self.done + clocks))
    }

    /// The colour code a palette output takes to the monitor: the EGA's six
    /// rgbRGB bits, or the VGA's DAC colour.
    fn colour_of(&self, index: u8) -> u32 {
        if !self.vga() {
            return u32::from(index & 0x3F);
        }
        let select = self.ac[0x14];
        let index = if self.ac[0x10] & ac_mode::P54 != 0 {
            (index & 0x0F) | ((select & 0x03) << 4) | ((select & 0x0C) << 4)
        } else {
            (index & 0x3F) | ((select & 0x0C) << 4)
        };
        self.dac_colour(index)
    }

    fn dac_colour(&self, index: u8) -> u32 {
        let [r, g, b] = self.dac[usize::from(index & self.dac_mask)];
        let scale = |c: u8| u32::from(c & 0x3F) * 255 / 63;
        (scale(r) << 16) | (scale(g) << 8) | scale(b)
    }

    /// The conversion the monitor applies to the card's codes now.
    fn conversion(&self) -> Conversion {
        if self.vga() {
            return match self.monitor {
                Monitor::Mono(phosphor) => Conversion::Luma(Shades::new(phosphor)),
                Monitor::Colour | Monitor::Composite => Conversion::Rgb,
            };
        }
        if let Monitor::Mono(phosphor) = self.monitor {
            // A monochrome display takes its video from the secondary blue
            // line and its intensity from the secondary green.
            let shades: [[u8; 3]; 64] = core::array::from_fn(|i| match (i & 0x08, i & 0x10) {
                (0, _) => [0; 3],
                (_, 0) => phosphor.normal(),
                _ => phosphor.bright(),
            });
            return Conversion::Palette(Palette::new(&shades));
        }
        let two_hundred = self.misc & 0x80 == 0;
        let colours: [[u8; 3]; 64] = core::array::from_fn(|i| {
            let i = u8::try_from(i).unwrap_or(0);
            if two_hundred {
                // The 5154 in its 200-line mode reads the secondary green as
                // intensity, as a 5153 would.
                CGA_COLOURS[usize::from((i & 7) | ((i >> 1) & 8))]
            } else {
                let level = |primary: u8, secondary: u8| {
                    (if i & primary != 0 { 0xAA } else { 0 })
                        + if i & secondary != 0 { 0x55 } else { 0 }
                };
                [level(4, 32), level(2, 16), level(1, 8)]
            }
        });
        Conversion::Palette(Palette::new(&colours))
    }

    /// The window a frame's timing implies.
    fn frame_window(&self) -> Window {
        let (left, width, top, height) = self.crtc.visible();
        let dots = self.dots_per_clock();
        let line = u32::from(self.crtc.line_clocks()) * dots;
        let lines = u32::from(self.crtc.frame_lines());
        Window {
            left: left * dots,
            width: (width * dots).clamp(1, 2048),
            top,
            height: height.clamp(1, 1024),
            hold_line: line - line / 4,
            free_line: (line + line / 4).max(64),
            hold_frame: lines - lines / 4,
            free_frame: (lines + lines / 4).max(64),
        }
    }

    /// Begins a frame: the window follows the timing, as a multisync
    /// monitor locks to each mode.
    fn new_frame(&mut self) {
        let window = self.frame_window();
        if window != *self.raster.window() {
            if let Ok(raster) = Raster::new(window, self.conversion()) {
                self.raster = raster;
            }
        }
        self.raster.vsync();
    }

    /// The emulated time the card has drawn to.
    #[must_use]
    pub fn drawn_to(&self) -> Nanos {
        self.clock.time_of(self.done)
    }

    /// Renders everything the beam has drawn by `t`.
    pub fn catch_up(&mut self, t: Nanos) {
        let target = self.clock.ticks_by(t);
        while self.done < target {
            let tick = self.crtc.step();
            self.done += 1;
            self.render(tick);
        }
    }

    fn render(&mut self, tick: Tick) {
        if tick.vsync && self.raster.locks_frame() {
            self.new_frame();
        }
        if tick.hsync {
            self.raster.hsync();
        }
        if tick.split && self.ac[0x10] & ac_mode::PANNING_RESET != 0 {
            self.panning.suspended = true;
        }
        if tick.vsync {
            self.panning.suspended = false;
        }
        self.last = tick;
        let dots = self.dots_per_clock();
        let mut cell = [0u32; 18];
        let shown = &mut cell[..dots as usize];
        if tick.blank || (self.vga() && self.seq[1] & 0x20 != 0) {
            self.raster.blank(dots);
            self.panning.displaying = false;
            return;
        }
        // While software holds the palette open the screen shows the border.
        let palette_open = self.ac_index & 0x20 == 0;
        if !tick.display || palette_open {
            if self.panning.displaying && self.panning.skip == 0 {
                // A panned line shows the start of the next character.
                let pan = self.pan();
                if pan > 0 {
                    self.cell(tick, shown);
                    self.raster
                        .displayed(&shown[..(pan as usize).min(shown.len())]);
                }
            }
            self.panning.displaying = false;
            let border = if self.vga() {
                self.dac_colour(self.ac[0x11])
            } else {
                u32::from(self.ac[0x11] & 0x3F)
            };
            shown.fill(border);
            self.raster.dots(shown);
            return;
        }
        if !self.panning.displaying {
            self.panning.skip = self.pan();
        }
        self.panning.displaying = true;
        self.cell(tick, shown);
        let drop = (self.panning.skip as usize).min(shown.len());
        self.panning.skip -= u32::try_from(drop).unwrap_or(0);
        self.raster.displayed(&shown[drop..]);
    }

    /// The pixel panning in dots for this part of the frame.
    fn pan(&self) -> u32 {
        if self.panning.suspended {
            return 0;
        }
        let pan = u32::from(self.ac[0x13] & 0x0F);
        let nine = self.seq[1] & 0x01 == 0 && self.ac[0x10] & ac_mode::GRAPHICS == 0;
        let pan = if nine { (pan + 1) % 9 } else { pan & 7 };
        if self.seq[1] & 0x08 != 0 {
            pan * 2
        } else {
            pan
        }
    }

    /// A character clock's dots.
    fn cell(&self, tick: Tick, out: &mut [u32]) {
        let bytes = self.planes.fetch(tick.addr);
        if self.shows_text() {
            self.text(tick, bytes, out);
            return;
        }
        // The serializer: eight four-bit values a character clock.
        let values: [u8; 8] = if self.vga() && self.gc[5] & gc_display::COLOUR_256 != 0 {
            core::array::from_fn(|i| {
                if i % 2 == 0 {
                    bytes[i / 2] >> 4
                } else {
                    bytes[i / 2] & 0x0F
                }
            })
        } else if self.gc[5] & gc_display::INTERLEAVE != 0 {
            core::array::from_fn(|i| (bytes[i / 4] >> (6 - 2 * (i % 4))) & 3)
        } else {
            core::array::from_fn(|i| {
                (0..4).fold(0, |colour, p| colour | (((bytes[p] >> (7 - i)) & 1) << p))
            })
        };
        let width = (out.len() / 8).max(1);
        let palette = |value: u8| self.ac[usize::from(value & self.ac[0x12] & 0x0F)];
        let eight_bit = self.vga() && self.ac[0x10] & ac_mode::EIGHT_BIT != 0;
        for (i, dot) in out.iter_mut().enumerate() {
            let at = (i / width).min(7);
            *dot = if eight_bit {
                // Two palette outputs' low nibbles make one 8-bit pixel,
                // shown for both of their dots.
                let pair = at & !1;
                self.dac_colour(
                    ((palette(values[pair]) & 0x0F) << 4) | (palette(values[pair + 1]) & 0x0F),
                )
            } else {
                self.colour_of(palette(values[at]))
            };
        }
    }

    fn text(&self, tick: Tick, bytes: [u8; 4], out: &mut [u32]) {
        let (code, attr) = (bytes[0], bytes[1]);
        let select = self.seq[3];
        let font = |n: u8| -> usize {
            if self.vga() {
                usize::from(n & 3) * 0x4000 + usize::from(n >> 2) * 0x2000
            } else {
                usize::from(n & 3) * 0x4000
            }
        };
        // Map A, chosen by attribute bit 3, is bits 5, 3 and 2; map B is
        // bits 4, 1 and 0. The EGA has only the low two of each.
        let (a, b) = if self.vga() {
            (
                ((select >> 2) & 3) | ((select >> 3) & 4),
                (select & 3) | ((select >> 2) & 4),
            )
        } else {
            ((select >> 2) & 3, select & 3)
        };
        let base = font(if attr & 0x08 != 0 { a } else { b });
        let mut row = self
            .planes
            .byte(2, base + usize::from(code) * 32 + usize::from(tick.row));
        let blink = self.ac[0x10] & ac_mode::BLINK != 0;
        let fields = self.crtc.fields();
        if blink && attr & 0x80 != 0 && fields & 0x10 == 0 {
            row = 0;
        }
        let mut ninth =
            self.ac[0x10] & ac_mode::LINE_GRAPHICS != 0 && joins_ninth_column(code) && row & 1 != 0;
        if (attr & 0x77 == 0x01 && tick.row == self.crtc.underline_row())
            || (tick.cursor && fields & 0x08 != 0)
        {
            row = 0xFF;
            ninth = true;
        }
        let fg = self.colour_of(self.ac[usize::from(attr & 0x0F)]);
        let background = if blink { (attr >> 4) & 0x07 } else { attr >> 4 };
        let bg = self.colour_of(self.ac[usize::from(background)]);
        let cell = if self.seq[1] & 0x01 != 0 { 8 } else { 9 };
        let width = out.len() / cell;
        for (i, dot) in out.iter_mut().enumerate() {
            let column = (i / width.max(1)).min(cell - 1);
            let lit = if column == 8 {
                ninth
            } else {
                row & (0x80 >> column) != 0
            };
            *dot = if lit { fg } else { bg };
        }
    }

    /// Reads a port at `t`.
    pub fn input(&mut self, port: u16, t: Nanos) -> u8 {
        self.catch_up(t);
        let colour = self.misc & 0x01 != 0;
        match port {
            0x3C3 if self.vga() => u8::from(self.enabled),
            _ if !self.enabled => 0xFF,
            0x3C0 if self.vga() => self.ac_index,
            0x3C1 if self.vga() => self
                .ac
                .get(usize::from(self.ac_index & 0x1F))
                .copied()
                .unwrap_or(0),
            0x3C2 => {
                let sense = match self.generation {
                    Generation::Ega => (EGA_SWITCHES >> (3 - ((self.misc >> 2) & 3))) & 1 != 0,
                    Generation::Vga => !matches!(self.monitor, Monitor::Mono(_)),
                };
                (u8::from(self.crtc.interrupt()) << 7) | (u8::from(sense) << 4)
            }
            0x3C4 if self.vga() => self.seq_index,
            0x3C5 if self.vga() => self
                .seq
                .get(usize::from(self.seq_index))
                .copied()
                .unwrap_or(0),
            0x3C6 if self.vga() => self.dac_mask,
            0x3C7 if self.vga() => {
                if self.dac_reading {
                    3
                } else {
                    0
                }
            }
            0x3C8 if self.vga() => self.dac_write,
            0x3C9 if self.vga() => {
                let value = self.dac[usize::from(self.dac_read)][usize::from(self.dac_component)];
                self.next_component(true);
                value
            }
            0x3CA if self.vga() => self.feature,
            0x3CC if self.vga() => self.misc,
            0x3CE if self.vga() => self.gc_index,
            0x3CF if self.vga() => self
                .gc
                .get(usize::from(self.gc_index))
                .copied()
                .unwrap_or(0),
            0x3B5 | 0x3D5 if (port == 0x3D5) == colour => self.crtc.read(),
            0x3B4 | 0x3D4 if self.vga() && (port == 0x3D4) == colour => self.crtc.selected(),
            0x3BA | 0x3DA if (port == 0x3DA) == colour => {
                self.ac_data_next = false;
                u8::from(self.crtc.not_displaying())
                    | (u8::from(self.crtc.in_vertical_retrace()) << 3)
            }
            _ => 0xFF,
        }
    }

    fn next_component(&mut self, reading: bool) {
        self.dac_component += 1;
        if self.dac_component == 3 {
            self.dac_component = 0;
            if reading {
                self.dac_read = self.dac_read.wrapping_add(1);
            } else {
                self.dac_write = self.dac_write.wrapping_add(1);
            }
        }
    }

    /// Writes a port at `t`; returns what moved.
    pub fn output(&mut self, port: u16, value: u8, t: Nanos) -> Moved {
        self.catch_up(t);
        let colour = self.misc & 0x01 != 0;
        let mut moved = Moved::default();
        match port {
            0x3C3 if self.vga() => {
                let enabled = value & 0x01 != 0;
                moved.ports = enabled != self.enabled;
                moved.window = moved.ports;
                self.enabled = enabled;
            }
            _ if !self.enabled => {}
            0x3C0 => {
                if self.ac_data_next {
                    let index = usize::from(self.ac_index & 0x1F);
                    if let Some(slot) = self.ac.get_mut(index) {
                        *slot = value;
                    }
                } else {
                    self.ac_index = value & 0x3F;
                }
                self.ac_data_next = !self.ac_data_next;
            }
            0x3C2 => {
                moved.ports = (self.misc ^ value) & 0x01 != 0;
                moved.window = (self.misc ^ value) & RAM_ENABLE != 0;
                self.misc = value;
                self.retime();
                self.raster.set_conversion(self.conversion());
            }
            0x3C4 => self.seq_index = value & 0x07,
            0x3C5 => {
                if let Some(slot) = self.seq.get_mut(usize::from(self.seq_index)) {
                    *slot = value;
                }
                self.retime();
            }
            0x3C6 if self.vga() => self.dac_mask = value,
            0x3C7 if self.vga() => {
                self.dac_read = value;
                self.dac_component = 0;
                self.dac_reading = true;
            }
            0x3C8 if self.vga() => {
                self.dac_write = value;
                self.dac_component = 0;
                self.dac_reading = false;
            }
            0x3C9 if self.vga() => {
                self.dac[usize::from(self.dac_write)][usize::from(self.dac_component)] =
                    value & 0x3F;
                self.next_component(false);
            }
            0x3CE => self.gc_index = value & 0x0F,
            0x3CF => {
                let before = self.window();
                if let Some(slot) = self.gc.get_mut(usize::from(self.gc_index)) {
                    *slot = value;
                }
                moved.window = self.window() != before;
            }
            0x3B4 | 0x3D4 if (port == 0x3D4) == colour => self.crtc.select(value),
            0x3B5 | 0x3D5 if (port == 0x3D5) == colour => self.crtc.write(value),
            0x3BA | 0x3DA if (port == 0x3DA) == colour => self.feature = value,
            _ => {}
        }
        moved
    }

    /// Reads a byte of the card's memory, loading the latches.
    pub fn read(&mut self, addr: u32) -> u8 {
        let routing = self.routing();
        self.planes.read(&routing, self.offset(addr))
    }

    /// What a read at `addr` would return, the latches untouched.
    #[must_use]
    pub fn peek(&self, addr: u32) -> u8 {
        self.planes.peek(&self.routing(), self.offset(addr))
    }

    /// Writes a byte of the card's memory, after the beam has drawn what it
    /// has passed.
    pub fn write(&mut self, addr: u32, value: u8, t: Nanos) {
        self.catch_up(t);
        let routing = self.routing();
        let offset = self.offset(addr);
        self.planes.write(&routing, offset, value);
    }

    /// Loads a character set into plane 2 at font slot `slot`, as firmware
    /// does: `height` rows of each of 256 glyphs in a 32-byte cell.
    pub fn load_font(&mut self, slot: u8, rows: impl Fn(u8, usize) -> u8, height: usize) {
        let base = if self.vga() {
            usize::from(slot & 3) * 0x4000 + usize::from(slot >> 2) * 0x2000
        } else {
            usize::from(slot & 3) * 0x4000
        };
        for code in 0..=255u8 {
            let glyph: [u8; 32] =
                core::array::from_fn(|row| if row < height { rows(code, row) } else { 0 });
            self.planes.load(2, base + usize::from(code) * 32, &glyph);
        }
    }

    /// Changes the monitor.
    pub fn set_monitor(&mut self, monitor: Monitor) {
        self.monitor = monitor;
        self.raster.set_conversion(self.conversion());
    }

    /// The last finished frame, if one finished since the last call.
    pub fn take_frame(&mut self) -> Option<Frame<'_>> {
        self.raster.take_frame()
    }

    /// Whether the graphics controller has the display read characters.
    #[must_use]
    pub fn shows_text(&self) -> bool {
        self.gc[6] & 0x01 == 0
    }

    /// The text the card shows: plane 0 at each displayed character clock.
    ///
    /// # Errors
    ///
    /// As [`super::Card::shown_text`].
    pub fn shown_text(&self) -> Result<ShownText, Unread> {
        if !self.shows_text() {
            return Err(Unread::NoText);
        }
        let columns = self.crtc.text_columns();
        let rows = u16::try_from(self.crtc.text_rows().count()).unwrap_or(u16::MAX);
        let codes = self.crtc.text_rows().flat_map(|(counter, scan)| {
            (0..columns).map(move |column| {
                let addr = self.crtc.text_address(counter, scan, column);
                self.planes.byte(0, usize::from(addr))
            })
        });
        read_text(columns, rows, codes)
    }
}

crate::snapshot::state!(Panning {
    skip,
    displaying,
    suspended,
});

crate::snapshot::state!(
    Vga {
        crtc,
        clock,
        done,
        planes,
        seq_index,
        seq,
        gc_index,
        gc,
        ac_index,
        ac_data_next,
        ac,
        misc,
        enabled,
        feature,
        dac,
        dac_write,
        dac_read,
        dac_component,
        dac_reading,
        dac_mask,
        raster,
        last,
        panning,
    }
    as built { generation, monitor }
    check |vga: &Vga| vga.dac_component < 3
);

#[cfg(test)]
#[path = "vga_tests.rs"]
mod tests;
