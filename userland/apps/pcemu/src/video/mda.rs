//! The IBM Monochrome Display Adapter, and the Hercules Graphics Card that
//! extends it with a 720×348 bitmap and a second page.
//!
//! Both run a 6845 at the 16.257 MHz dot clock: nine dots a character in
//! text, sixteen a character clock in Hercules graphics.

use super::beam::Beam;
use super::crtc6845::Tick;
use super::font::{joins_ninth_column, EGA as FONT};
use super::monitor::{mono, mono_conversion, Phosphor};
use super::raster::{Raster, RasterError, Window};
use super::{read_text, Frame, ShownText, Unread};
use crate::time::{Hertz, Nanos};
use alloc::vec::Vec;

/// The 5151's view of the raster, wide and tall enough for both the text
/// and the Hercules graphics timing.
const WINDOW: Window = Window {
    left: 119,
    width: 754,
    top: 16,
    height: 358,
    hold_line: 780,
    free_line: 1000,
    hold_frame: 310,
    free_frame: 450,
};

/// The frame the 5151 shows: its window's dots and lines.
pub const FRAME: (u32, u32) = (WINDOW.width, WINDOW.height);

/// Where the card's memory starts.
pub const MEMORY_BASE: u32 = 0xB_0000;
/// The window the MDA decodes, its 4 KiB repeating through it.
const MDA_WINDOW: u32 = 0x8000;

/// The mode control register's bits.
mod mode {
    pub const HIGH_RES: u8 = 0x01;
    pub const GRAPHICS: u8 = 0x02;
    pub const VIDEO: u8 = 0x08;
    pub const BLINK: u8 = 0x20;
    pub const PAGE: u8 = 0x80;
}

/// The Hercules configuration switch's bits.
mod config {
    pub const ALLOW_GRAPHICS: u8 = 0x01;
    pub const ALLOW_PAGE_1: u8 = 0x02;
}

/// The row an underline lights.
const UNDERLINE_ROW: u8 = 12;

/// Which card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// The IBM MDA: 4 KiB, text only.
    Mda,
    /// The Hercules Graphics Card: 64 KiB in two pages, and graphics.
    Hercules,
}

/// The card.
#[derive(Clone, Debug)]
pub struct Mda {
    model: Model,
    beam: Beam,
    vram: Vec<u8>,
    mode: u8,
    config: u8,
    raster: Raster,
    /// The last character clock's sync and video lines, which the status
    /// port reports.
    last: Tick,
    lit: bool,
    /// Vertical syncs counted, which time the cursor's and characters'
    /// blinking.
    fields: u32,
}

impl Mda {
    /// A card of `model` on a monitor of `phosphor`, as power-up leaves it.
    pub fn new(model: Model, phosphor: Phosphor) -> Result<Self, RasterError> {
        let size = match model {
            Model::Mda => 0x1000,
            Model::Hercules => 0x1_0000,
        };
        let mut vram = Vec::new();
        vram.try_reserve_exact(size)
            .map_err(|_| RasterError::OutOfMemory)?;
        vram.resize(size, 0);
        let beam = Beam::new(Self::char_rate(0));
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
            model,
            beam,
            vram,
            mode: 0,
            config: 0,
            raster: Raster::new(WINDOW, mono_conversion(phosphor))?,
            last,
            lit: false,
            fields: 0,
        })
    }

    fn graphics(&self) -> bool {
        self.model == Model::Hercules
            && self.mode & mode::GRAPHICS != 0
            && self.config & config::ALLOW_GRAPHICS != 0
    }

    /// Character clocks a second: a nine-dot cell in text, sixteen dots in
    /// graphics.
    fn char_rate(dots: u64) -> Hertz {
        Hertz::MONO_DOTS
            .divided(if dots == 16 { 16 } else { 9 })
            .unwrap_or(Hertz::MONO_DOTS)
    }

    /// The I/O ranges the card decodes; the printer port shares its block.
    #[must_use]
    pub fn ports(&self) -> &'static [(u16, u16)] {
        match self.model {
            Model::Mda => &[(0x3B0, 0x3BB)],
            Model::Hercules => &[(0x3B0, 0x3BB), (0x3BF, 0x3BF)],
        }
    }

    /// The memory the card answers: its base and length.
    #[must_use]
    pub fn window(&self) -> (u32, u32) {
        match self.model {
            Model::Hercules if self.config & config::ALLOW_PAGE_1 != 0 => (MEMORY_BASE, 0x1_0000),
            _ => (MEMORY_BASE, MDA_WINDOW),
        }
    }

    fn offset(&self, addr: u32) -> usize {
        (addr.wrapping_sub(MEMORY_BASE) as usize) & (self.vram.len() - 1)
    }

    /// Where the page the display shows begins in the card's memory.
    fn page(&self) -> usize {
        if self.model == Model::Hercules && self.mode & mode::PAGE != 0 {
            0x8000
        } else {
            0
        }
    }

    /// The emulated time the card has drawn to.
    #[must_use]
    pub fn drawn_to(&self) -> Nanos {
        self.beam.drawn_to()
    }

    /// Renders everything the beam has drawn by `t`.
    pub fn catch_up(&mut self, t: Nanos) {
        let graphics = self.graphics();
        let page = self.page();
        let Self {
            beam,
            vram,
            mode,
            raster,
            last,
            lit,
            model,
            fields,
            ..
        } = self;
        // The MDA's display runs only in its high-resolution mode; the
        // Hercules card has no such bit.
        let on =
            *mode & mode::VIDEO != 0 && (*model == Model::Hercules || *mode & mode::HIGH_RES != 0);
        let mask = vram.len() - 1;
        beam.run_to(t, |tick| {
            if tick.vsync {
                raster.vsync();
                *fields = fields.wrapping_add(1);
            }
            if tick.hsync {
                raster.hsync();
            }
            *last = tick;
            if !(tick.display && on) {
                *lit = false;
                raster.blank(if graphics { 16 } else { 9 });
                return;
            }
            if graphics {
                let at =
                    page + (usize::from(tick.ra & 3) << 13) + ((usize::from(tick.ma) * 2) & 0x1FFF);
                let bits = u16::from_be_bytes([vram[at & mask], vram[(at + 1) & mask]]);
                let mut dots = [mono::DARK; 16];
                for (i, dot) in dots.iter_mut().enumerate() {
                    if bits & (0x8000 >> i) != 0 {
                        *dot = mono::NORMAL;
                    }
                }
                *lit = bits & 1 != 0;
                raster.displayed(&dots);
            } else {
                let at = cell_at(page, tick.ma, mask);
                let dots = text_cell(vram[at], vram[(at + 1) & mask], tick, *mode, *fields);
                *lit = dots[8] != mono::DARK;
                raster.displayed(&dots);
            }
        });
    }

    /// Reads a port.
    pub fn input(&mut self, port: u16, t: Nanos) -> u8 {
        self.catch_up(t);
        match port {
            0x3B1 | 0x3B3 | 0x3B5 | 0x3B7 => self.beam.crtc.read(),
            0x3BA => {
                let mut status = u8::from(self.last.in_hsync) | (u8::from(self.lit) << 3);
                status |= match self.model {
                    // Unused bits float high.
                    Model::Mda => 0xF6,
                    // Bit 7 drops during vertical retrace; bits 6-4 identify
                    // the original card.
                    Model::Hercules => 0x06 | if self.last.in_vsync { 0 } else { 0x80 },
                };
                status
            }
            _ => 0xFF,
        }
    }

    /// Writes a port; returns whether the memory window changed.
    pub fn output(&mut self, port: u16, value: u8, t: Nanos) -> bool {
        self.catch_up(t);
        match port {
            0x3B0 | 0x3B2 | 0x3B4 | 0x3B6 => self.beam.crtc.select(value),
            0x3B1 | 0x3B3 | 0x3B5 | 0x3B7 => self.beam.crtc.write(value),
            0x3B8 => {
                self.mode = value;
                self.beam
                    .set_rate(Self::char_rate(if self.graphics() { 16 } else { 9 }));
            }
            0x3BF if self.model == Model::Hercules => {
                let before = self.window();
                self.config = value & (config::ALLOW_GRAPHICS | config::ALLOW_PAGE_1);
                self.beam
                    .set_rate(Self::char_rate(if self.graphics() { 16 } else { 9 }));
                return self.window() != before;
            }
            _ => {}
        }
        false
    }

    /// Reads a byte of the card's memory.
    #[must_use]
    pub fn read(&self, addr: u32) -> u8 {
        self.vram[self.offset(addr)]
    }

    /// Writes a byte of the card's memory, after the beam has drawn what it
    /// has passed.
    pub fn write(&mut self, addr: u32, value: u8, t: Nanos) {
        self.catch_up(t);
        let offset = self.offset(addr);
        self.vram[offset] = value;
    }

    /// Changes the monitor's phosphor.
    pub fn set_phosphor(&mut self, phosphor: Phosphor) {
        self.raster.set_conversion(mono_conversion(phosphor));
    }

    /// The last finished frame, if one finished since the last call.
    pub fn take_frame(&mut self) -> Option<Frame<'_>> {
        self.raster.take_frame()
    }

    /// Whether the card is in a text mode.
    #[must_use]
    pub fn shows_text(&self) -> bool {
        !self.graphics()
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
        let (page, mask) = (self.page(), self.vram.len() - 1);
        let codes = (0..rows).flat_map(|row| {
            (0..columns)
                .map(move |column| self.vram[cell_at(page, crtc.cell_address(row, column), mask)])
        });
        read_text(columns, rows, codes)
    }
}

/// Where the code of the character at counter `ma` lies on the page at
/// `page` of memory masked by `mask`; its attribute follows it.
fn cell_at(page: usize, ma: u16, mask: usize) -> usize {
    (page + ((usize::from(ma) * 2) & 0x7FFF)) & mask
}

/// One text character's nine dots: the glyph's row, its ninth column, the
/// MDA's attribute rules, blinking and the cursor.
fn text_cell(code: u8, attr: u8, tick: Tick, mode: u8, fields: u32) -> [u32; 9] {
    let row = FONT.row(code, usize::from(tick.ra));
    let mut bits = (u16::from(row) << 1)
        | if joins_ninth_column(code) {
            u16::from(row & 1)
        } else {
            0
        };
    let blink_enabled = mode & mode::BLINK != 0;
    let (fg, bg) = match attr & 0x77 {
        0x00 => (mono::DARK, mono::DARK),
        0x70 => (
            mono::DARK,
            if !blink_enabled && attr & 0x80 != 0 {
                mono::BRIGHT
            } else {
                mono::NORMAL
            },
        ),
        _ => (
            if attr & 0x08 != 0 {
                mono::BRIGHT
            } else {
                mono::NORMAL
            },
            mono::DARK,
        ),
    };
    if attr & 0x07 == 0x01 && tick.ra == UNDERLINE_ROW {
        bits = 0x1FF;
    }
    if blink_enabled && attr & 0x80 != 0 && fields & 0x10 == 0 {
        bits = 0;
    }
    let cursor = tick.cursor && fields & 0x08 != 0;
    let mut dots = [bg; 9];
    for (i, dot) in dots.iter_mut().enumerate() {
        if cursor {
            *dot = if fg == mono::DARK { mono::NORMAL } else { fg };
        } else if bits & (0x100 >> i) != 0 {
            *dot = fg;
        }
    }
    dots
}

crate::snapshot::state!(
    Mda { beam, vram, mode, config, raster, last, lit, fields }
    as built { model }
);

#[cfg(test)]
#[path = "mda_tests.rs"]
mod tests;
