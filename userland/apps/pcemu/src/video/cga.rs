//! The IBM Color Graphics Adapter.
//!
//! A 6845 at the 14.318 MHz dot clock, divided by eight in 80-column text and
//! by sixteen otherwise. Every mode fills the same 640-dot line: a 40-column
//! character or two graphics bytes is sixteen dots, its pixels doubled in
//! 320-pixel modes.

use super::beam::Beam;
use super::crtc6845::Tick;
use super::font::CGA as FONT;
use super::monitor::{rgbi_conversion, Monitor};
use super::raster::{Raster, RasterError, Window};
use super::{composite, read_text, Frame, ShownText, Unread};
use crate::time::{Hertz, Nanos};
use alloc::vec::Vec;

/// The 5153's view of the raster.
const WINDOW: Window = Window {
    left: 128,
    width: 768,
    top: 22,
    height: 232,
    hold_line: 800,
    free_line: 1024,
    hold_frame: 220,
    free_frame: 320,
};

/// The frame the 5153 shows: its window's dots and lines.
pub const FRAME: (u32, u32) = (WINDOW.width, WINDOW.height);

/// Where the card's memory starts.
pub const MEMORY_BASE: u32 = 0xB_8000;
/// The window it decodes, its 16 KiB repeating through it.
const MEMORY_WINDOW: u32 = 0x8000;
const VRAM: usize = 0x4000;

/// The mode control register's bits.
mod mode {
    pub const HIGH_RES_TEXT: u8 = 0x01;
    pub const GRAPHICS: u8 = 0x02;
    pub const MONO: u8 = 0x04;
    pub const VIDEO: u8 = 0x08;
    pub const HIGH_RES_GRAPHICS: u8 = 0x10;
    pub const BLINK: u8 = 0x20;
}

/// The colour select register's bits.
mod colour {
    pub const BORDER: u8 = 0x0F;
    pub const INTENSE: u8 = 0x10;
    pub const PALETTE: u8 = 0x20;
}

/// Dots a CPU access waits through at most: the memory's sixteen-dot cycle,
/// which it shares with the display.
const SLOT_DOTS: u64 = 16;

/// The card.
#[derive(Clone, Debug)]
pub struct Cga {
    beam: Beam,
    vram: Vec<u8>,
    mode: u8,
    colour: u8,
    monitor: Monitor,
    raster: Raster,
    last: Tick,
    fields: u32,
    pen_latched: bool,
}

impl Cga {
    /// A card on `monitor`, as power-up leaves it.
    pub fn new(monitor: Monitor) -> Result<Self, RasterError> {
        let mut vram = Vec::new();
        vram.try_reserve_exact(VRAM)
            .map_err(|_| RasterError::OutOfMemory)?;
        vram.resize(VRAM, 0);
        let last = Tick {
            ma: 0,
            ra: 0,
            display: false,
            cursor: false,
            hsync: false,
            vsync: false,
            in_hsync: false,
            in_vsync: false,
        };
        Ok(Self {
            beam: Beam::new(Self::char_rate(0)),
            vram,
            mode: 0,
            colour: 0,
            monitor,
            raster: Raster::new(WINDOW, Self::conversion(monitor, 0))?,
            last,
            fields: 0,
            pen_latched: false,
        })
    }

    fn conversion(monitor: Monitor, mode: u8) -> super::raster::Conversion {
        match monitor {
            Monitor::Composite => composite::conversion(mode & mode::MONO == 0, WINDOW.left),
            other => rgbi_conversion(other),
        }
    }

    fn char_rate(mode: u8) -> Hertz {
        let divider = if mode & mode::HIGH_RES_TEXT != 0 {
            8
        } else {
            16
        };
        Hertz::CRYSTAL.divided(divider).unwrap_or(Hertz::CRYSTAL)
    }

    /// The I/O range the card decodes.
    pub const PORTS: &'static [(u16, u16)] = &[(0x3D0, 0x3DF)];

    /// The memory the card answers: its base and length.
    #[must_use]
    pub const fn window() -> (u32, u32) {
        (MEMORY_BASE, MEMORY_WINDOW)
    }

    /// The emulated time the card has drawn to.
    #[must_use]
    pub fn drawn_to(&self) -> Nanos {
        self.beam.drawn_to()
    }

    /// Renders everything the beam has drawn by `t`.
    pub fn catch_up(&mut self, t: Nanos) {
        let Self {
            beam,
            vram,
            mode,
            colour,
            raster,
            last,
            fields,
            ..
        } = self;
        let (mode, colour) = (*mode, *colour);
        let wide = mode & mode::HIGH_RES_TEXT == 0;
        let dots = if wide { 16 } else { 8 };
        beam.run_to(t, |tick| {
            if tick.vsync {
                raster.vsync();
                *fields = fields.wrapping_add(1);
            }
            if tick.hsync {
                raster.hsync();
            }
            *last = tick;
            let mut cell = [0u32; 16];
            let cell = &mut cell[..dots];
            if mode & mode::VIDEO == 0 {
                raster.blank(u32::try_from(dots).unwrap_or(16));
                return;
            }
            if !tick.display {
                let border = if mode & (mode::GRAPHICS | mode::HIGH_RES_GRAPHICS)
                    == mode::GRAPHICS | mode::HIGH_RES_GRAPHICS
                {
                    0
                } else {
                    colour & colour::BORDER
                };
                cell.fill(u32::from(border));
                raster.dots(cell);
                return;
            }
            if mode & mode::GRAPHICS != 0 {
                graphics(vram, tick, mode, colour, cell);
            } else {
                text(vram, tick, mode, *fields, wide, cell);
            }
            raster.displayed(cell);
        });
    }

    /// Reads a port.
    pub fn input(&mut self, port: u16, t: Nanos) -> u8 {
        self.catch_up(t);
        match port & 0x0F {
            0x1 | 0x3 | 0x5 | 0x7 => self.beam.crtc.read(),
            0xA => {
                // No pen is attached, so its switch reads open.
                0xF4 | u8::from(!self.last.display)
                    | (u8::from(self.pen_latched) << 1)
                    | (u8::from(self.last.in_vsync) << 3)
            }
            _ => 0xFF,
        }
    }

    /// Writes a port.
    pub fn output(&mut self, port: u16, value: u8, t: Nanos) {
        self.catch_up(t);
        match port & 0x0F {
            0x0 | 0x2 | 0x4 | 0x6 => self.beam.crtc.select(value),
            0x1 | 0x3 | 0x5 | 0x7 => self.beam.crtc.write(value),
            0x8 => {
                let burst = (self.mode ^ value) & mode::MONO != 0;
                self.mode = value;
                self.beam.set_rate(Self::char_rate(value));
                if burst {
                    self.raster
                        .set_conversion(Self::conversion(self.monitor, value));
                }
            }
            0x9 => self.colour = value,
            0xB => self.pen_latched = false,
            0xC if !self.pen_latched => {
                self.beam.crtc.strobe_light_pen();
                self.pen_latched = true;
            }
            _ => {}
        }
    }

    /// Reads a byte of the card's memory.
    #[must_use]
    pub fn read(&self, addr: u32) -> u8 {
        self.vram[addr as usize % VRAM]
    }

    /// Writes a byte of the card's memory, after the beam has drawn what it
    /// has passed.
    pub fn write(&mut self, addr: u32, value: u8, t: Nanos) {
        self.catch_up(t);
        self.vram[addr as usize % VRAM] = value;
    }

    /// How long a CPU access made at `t` waits for the card's memory: until
    /// its next sixteen-dot cycle begins.
    #[must_use]
    pub fn access_wait(t: Nanos) -> Nanos {
        let dot = Hertz::CRYSTAL.ticks_at(t);
        let slot = (dot / SLOT_DOTS + 1) * SLOT_DOTS;
        Hertz::CRYSTAL.time_of(slot).saturating_sub(t)
    }

    /// Changes the monitor.
    pub fn set_monitor(&mut self, monitor: Monitor) {
        self.monitor = monitor;
        self.raster
            .set_conversion(Self::conversion(monitor, self.mode));
    }

    /// The last finished frame, if one finished since the last call.
    pub fn take_frame(&mut self) -> Option<Frame<'_>> {
        self.raster.take_frame()
    }

    /// Whether the card is in a text mode.
    #[must_use]
    pub const fn shows_text(&self) -> bool {
        self.mode & mode::GRAPHICS == 0
    }

    /// The text the card shows.
    ///
    /// # Errors
    ///
    /// As [`super::Card::shown_text`].
    pub fn shown_text(&self) -> Result<ShownText, Unread> {
        if !self.shows_text() {
            return Err(Unread::NoText);
        }
        let crtc = &self.beam.crtc;
        let (columns, rows) = crtc.text_shape();
        let codes = (0..rows).flat_map(|row| {
            (0..columns).map(move |column| self.vram[cell_at(crtc.cell_address(row, column))])
        });
        read_text(columns, rows, codes)
    }
}

/// Where the code of the character at counter `ma` lies; its attribute
/// follows it.
fn cell_at(ma: u16) -> usize {
    (usize::from(ma) * 2) % VRAM
}

/// A text character's dots: eight, or sixteen with each doubled in 40
/// columns.
fn text(vram: &[u8], tick: Tick, mode: u8, fields: u32, wide: bool, cell: &mut [u32]) {
    let at = cell_at(tick.ma);
    let (code, attr) = (vram[at], vram[(at + 1) % VRAM]);
    let blink_enabled = mode & mode::BLINK != 0;
    let fg = u32::from(attr & 0x0F);
    let bg = u32::from(if blink_enabled {
        (attr >> 4) & 0x07
    } else {
        attr >> 4
    });
    let mut row = FONT.row(code, usize::from(tick.ra & 7));
    if blink_enabled && attr & 0x80 != 0 && fields & 0x10 == 0 {
        row = 0;
    }
    if tick.cursor && fields & 0x08 != 0 {
        row = 0xFF;
    }
    for (i, dot) in cell.iter_mut().enumerate() {
        let pixel = if wide { i / 2 } else { i };
        *dot = if row & (0x80 >> pixel) != 0 { fg } else { bg };
    }
}

/// Two graphics bytes' dots, from the even or odd bank by the row address.
/// The serializer shifts at the pixel rate whatever the controller's clock:
/// a 320-mode pixel is two dots, a 640-mode pixel one, so a character clock
/// of eight dots shows only the first byte.
fn graphics(vram: &[u8], tick: Tick, mode: u8, colour: u8, cell: &mut [u32]) {
    let at = (usize::from(tick.ra & 1) << 13) | ((usize::from(tick.ma) * 2) & 0x1FFF);
    let bits = u16::from_be_bytes([vram[at], vram[(at + 1) % VRAM]]);
    if mode & mode::HIGH_RES_GRAPHICS != 0 {
        let foreground = u32::from(colour & colour::BORDER);
        for (i, dot) in cell.iter_mut().enumerate() {
            *dot = if bits & (0x8000 >> i) != 0 {
                foreground
            } else {
                0
            };
        }
        return;
    }
    let intense = if colour & colour::INTENSE != 0 { 8 } else { 0 };
    let palette: [u8; 3] = if mode & mode::MONO != 0 {
        [3, 4, 7]
    } else if colour & colour::PALETTE != 0 {
        [3, 5, 7]
    } else {
        [2, 4, 6]
    };
    for (i, dot) in cell.iter_mut().enumerate() {
        let value = (bits >> (14 - 2 * (i / 2))) & 3;
        *dot = u32::from(match value {
            0 => colour & colour::BORDER,
            n => palette[usize::from(n) - 1] | intense,
        });
    }
}

crate::snapshot::state!(
    Cga { beam, vram, mode, colour, raster, last, fields, pen_latched }
    as built { monitor }
);

#[cfg(test)]
#[path = "cga_tests.rs"]
mod tests;
