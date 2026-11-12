//! A monitor's view of a card's dot stream.
//!
//! A CRT's deflection locks to the sync pulses, so the visible window is
//! measured from them: it starts a fixed number of dots after each horizontal
//! sync begins and a fixed number of lines after each vertical sync begins.
//! Reprogrammed sync timing moves the picture as it would on the monitor. Each
//! oscillator follows a pulse only within its hold range: one that comes too
//! soon is ignored, and with none in time the oscillator starts the line or
//! the frame itself.
//!
//! Dots arrive as codes in the card's colour space. Each line becomes RGBA
//! when it ends, through the monitor's [`Conversion`].

use super::composite::Composite;
use crate::snapshot::{check, restore_bytes, save_bytes, In, Out, State, Torn};
use alloc::vec::Vec;

/// Bytes in an RGBA pixel.
const RGBA: usize = 4;

/// Where the visible picture sits relative to the sync pulses, in the card's
/// dots and lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// Dots from the start of horizontal sync to the window's left edge.
    pub left: u32,
    /// Visible dots per line.
    pub width: u32,
    /// Lines from the start of vertical sync to the window's top edge.
    pub top: u32,
    /// Visible lines.
    pub height: u32,
    /// The shortest line a horizontal sync can end.
    pub hold_line: u32,
    /// The longest line the monitor's oscillator lets run without sync.
    pub free_line: u32,
    /// The fewest lines a vertical sync can end a frame after.
    pub hold_frame: u32,
    /// The most lines a frame runs without vertical sync.
    pub free_frame: u32,
}

/// The part of a frame its card displayed — the picture inside the border —
/// in the frame's own pixels, half-open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Area {
    /// The first displayed dot of a line.
    pub left: u32,
    /// The first displayed line.
    pub top: u32,
    /// One past the last displayed dot of a line.
    pub right: u32,
    /// One past the last displayed line.
    pub bottom: u32,
}

impl Area {
    fn joined(this: Option<Self>, other: Self) -> Self {
        this.map_or(other, |a| Self {
            left: a.left.min(other.left),
            top: a.top.min(other.top),
            right: a.right.max(other.right),
            bottom: a.bottom.max(other.bottom),
        })
    }
}

/// How a line's codes become colours.
#[derive(Clone, Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "built once a mode change and held by value; boxing would only add an allocation"
)]
pub enum Conversion {
    /// Codes are `0x00RRGGBB`.
    Rgb,
    /// Codes index a palette of up to 64 colours.
    Palette(Palette),
    /// Codes are a CGA's RGBI dots on a composite monitor.
    Composite(Composite),
    /// Codes are `0x00RRGGBB`, shown by their luminance in a phosphor's
    /// shades.
    Luma(Shades),
}

/// A phosphor's 256 shades, as RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shades([[u8; RGBA]; 256]);

impl Shades {
    /// The shades of `phosphor`, dark to full drive.
    #[must_use]
    pub fn new(phosphor: super::monitor::Phosphor) -> Self {
        Self(core::array::from_fn(|level| {
            let [r, g, b] = phosphor.at(u8::try_from(level).unwrap_or(u8::MAX));
            [r, g, b, 0xFF]
        }))
    }
}

/// Up to 64 colours, as RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette([[u8; RGBA]; 64]);

impl Palette {
    /// A palette of `colours`, the rest black.
    #[must_use]
    pub fn new(colours: &[[u8; 3]]) -> Self {
        let mut table = [[0, 0, 0, 0xFF]; 64];
        for (slot, &[r, g, b]) in table.iter_mut().zip(colours) {
            *slot = [r, g, b, 0xFF];
        }
        Self(table)
    }

    fn colour(&self, code: u32) -> [u8; RGBA] {
        self.0[(code & 63) as usize]
    }
}

impl Conversion {
    fn line(&self, codes: &[u32], out: &mut [u8]) {
        let pixels = codes.iter().zip(out.as_chunks_mut::<RGBA>().0.iter_mut());
        match self {
            Self::Rgb => {
                for (&code, pixel) in pixels {
                    let [b, g, r, _] = code.to_le_bytes();
                    pixel.copy_from_slice(&[r, g, b, 0xFF]);
                }
            }
            Self::Palette(palette) => {
                for (&code, pixel) in pixels {
                    pixel.copy_from_slice(&palette.colour(code));
                }
            }
            Self::Composite(composite) => composite.line(codes, out),
            Self::Luma(shades) => {
                for (&code, pixel) in pixels {
                    let [b, g, r, _] = code.to_le_bytes();
                    let luma =
                        (299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b)) / 1000;
                    pixel.copy_from_slice(&shades.0[luma.min(255) as usize]);
                }
            }
        }
    }
}

/// Why a raster could not be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RasterError {
    /// The window is empty.
    Empty,
    /// Its buffers could not be allocated.
    OutOfMemory,
}

/// The monitor: the beam's place, the frame it is drawing and the last one
/// it finished.
#[derive(Clone, Debug)]
pub struct Raster {
    window: Window,
    conversion: Conversion,
    line: Vec<u32>,
    drawing: Vec<u8>,
    shown: Vec<u8>,
    fresh: bool,
    dot: u32,
    row: u32,
    /// Window rows the frame being drawn has reached.
    reached: u32,
    /// What the frame being drawn has displayed, and what the last finished
    /// one did.
    displaying: Option<Area>,
    displayed: Option<Area>,
}

fn zeroed<T: Clone + Default>(len: usize) -> Result<Vec<T>, RasterError> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(len)
        .map_err(|_| RasterError::OutOfMemory)?;
    buffer.resize(len, T::default());
    Ok(buffer)
}

impl Raster {
    /// A raster for `window`, its frames black until the first completes.
    pub fn new(window: Window, conversion: Conversion) -> Result<Self, RasterError> {
        if window.width == 0 || window.height == 0 {
            return Err(RasterError::Empty);
        }
        let pixels = window.width as usize * window.height as usize * RGBA;
        Ok(Self {
            line: zeroed(window.width as usize)?,
            drawing: zeroed(pixels)?,
            shown: zeroed(pixels)?,
            window,
            conversion,
            fresh: false,
            dot: 0,
            row: 0,
            reached: 0,
            displaying: None,
            displayed: None,
        })
    }

    /// The visible window.
    #[must_use]
    pub const fn window(&self) -> &Window {
        &self.window
    }

    /// Replaces the conversion from the next line on.
    pub fn set_conversion(&mut self, conversion: Conversion) {
        self.conversion = conversion;
    }

    /// Takes the border's dots `codes` at the beam, moving it on.
    pub fn dots(&mut self, codes: &[u32]) {
        self.take(codes, false);
    }

    /// Takes the picture's dots `codes` at the beam, moving it on: the dots
    /// inside the border.
    pub fn displayed(&mut self, codes: &[u32]) {
        self.take(codes, true);
    }

    fn take(&mut self, codes: &[u32], picture: bool) {
        let mut rest = codes;
        while !rest.is_empty() {
            let room = self.window.free_line.saturating_sub(self.dot).max(1);
            let (now, later) = rest.split_at(rest.len().min(room as usize));
            self.place(now, picture);
            rest = later;
            if self.dot >= self.window.free_line {
                self.hsync();
            }
        }
    }

    /// Advances the beam over `dots` dots the card blanks.
    pub fn blank(&mut self, dots: u32) {
        let mut rest = dots;
        while rest > 0 {
            let step = rest.min(self.window.free_line.saturating_sub(self.dot).max(1));
            self.dot += step;
            rest -= step;
            if self.dot >= self.window.free_line {
                self.hsync();
            }
        }
    }

    fn place(&mut self, codes: &[u32], picture: bool) {
        let start = self.dot;
        self.dot += u32::try_from(codes.len()).unwrap_or(u32::MAX);
        let left = self.window.left;
        let right = left + self.window.width;
        let (from, to) = (start.max(left), self.dot.min(right));
        if from < to {
            let src = &codes[(from - start) as usize..(to - start) as usize];
            self.line[(from - left) as usize..(to - left) as usize].copy_from_slice(src);
            let top = self.window.top;
            if picture && (top..top + self.window.height).contains(&self.row) {
                let row = self.row - top;
                self.displaying = Some(Area::joined(
                    self.displaying,
                    Area {
                        left: from - left,
                        top: row,
                        right: to - left,
                        bottom: row + 1,
                    },
                ));
            }
        }
    }

    /// Horizontal sync begins: the line ends, if the oscillator follows it.
    pub fn hsync(&mut self) {
        if self.dot < self.window.hold_line {
            return;
        }
        let top = self.window.top;
        if (top..top + self.window.height).contains(&self.row) {
            let index = (self.row - top) as usize;
            let stride = self.window.width as usize * RGBA;
            let out = &mut self.drawing[index * stride..(index + 1) * stride];
            self.conversion.line(&self.line, out);
            self.reached = self.reached.max(self.row - top + 1);
        }
        self.line.fill(0);
        self.dot = 0;
        self.row += 1;
        if self.row >= self.window.free_frame {
            self.vsync();
        }
    }

    /// Whether a vertical sync now would end the frame.
    #[must_use]
    pub const fn locks_frame(&self) -> bool {
        self.row >= self.window.hold_frame
    }

    /// Vertical sync begins: the frame ends, if the oscillator follows it,
    /// and whatever of the window the beam did not reach is black.
    pub fn vsync(&mut self) {
        if !self.locks_frame() {
            return;
        }
        let stride = self.window.width as usize * RGBA;
        let reached = self.reached as usize * stride;
        for pixel in self.drawing[reached..].as_chunks_mut::<RGBA>().0 {
            pixel.copy_from_slice(&[0, 0, 0, 0xFF]);
        }
        core::mem::swap(&mut self.drawing, &mut self.shown);
        self.displayed = self.displaying.take();
        self.fresh = true;
        self.row = 0;
        self.reached = 0;
    }

    /// The last finished frame, if one finished since the last call.
    pub fn take_frame(&mut self) -> Option<super::Frame<'_>> {
        core::mem::take(&mut self.fresh).then_some(super::Frame {
            width: self.window.width,
            height: self.window.height,
            pixels: self.shown.as_slice(),
            displayed: self.displayed,
        })
    }

    /// The last finished frame, whether or not it was taken.
    #[must_use]
    pub fn frame(&self) -> &[u8] {
        &self.shown
    }
}

crate::snapshot::state!(Window {
    left,
    width,
    top,
    height,
    hold_line,
    free_line,
    hold_frame,
    free_frame,
});

crate::snapshot::state!(Area {
    left,
    top,
    right,
    bottom,
});

impl State for Conversion {
    fn save(&self, out: &mut Out) {
        match self {
            Self::Rgb => out.u8(0),
            Self::Palette(Palette(table)) => {
                out.u8(1);
                table.save(out);
            }
            Self::Composite(composite) => {
                out.u8(2);
                composite.save(out);
            }
            Self::Luma(Shades(table)) => {
                out.u8(3);
                table.save(out);
            }
        }
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        *self = match input.u8()? {
            0 => Self::Rgb,
            1 => {
                let mut table = [[0; RGBA]; 64];
                table.restore(input)?;
                Self::Palette(Palette(table))
            }
            2 => {
                let mut composite = Composite {
                    burst: false,
                    phase: 0,
                };
                composite.restore(input)?;
                Self::Composite(composite)
            }
            3 => {
                let mut table = [[0; RGBA]; 256];
                table.restore(input)?;
                Self::Luma(Shades(table))
            }
            _ => return Err(Torn::Malformed),
        };
        Ok(())
    }
}

/// Whether a raster could draw `window`: a frame some card makes, its edges
/// inside the beam's reach.
fn drawable(window: &Window) -> bool {
    let (most_w, most_h) = super::LARGEST_FRAME;
    (1..=most_w).contains(&window.width)
        && (1..=most_h).contains(&window.height)
        && window.left.checked_add(window.width).is_some()
        && window.top.checked_add(window.height).is_some()
}

impl State for Raster {
    fn save(&self, out: &mut Out) {
        let Self {
            window,
            conversion,
            line,
            drawing,
            shown,
            fresh,
            dot,
            row,
            reached,
            displaying,
            displayed,
        } = self;
        window.save(out);
        conversion.save(out);
        line.save(out);
        save_bytes(drawing, out);
        save_bytes(shown, out);
        fresh.save(out);
        dot.save(out);
        row.save(out);
        reached.save(out);
        displaying.save(out);
        displayed.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        let mut window = self.window;
        window.restore(input)?;
        check(drawable(&window))?;
        if window != self.window {
            // The card had moved to a mode drawn in another window.
            *self = Self::new(window, Conversion::Rgb).map_err(|_| Torn::Mismatch)?;
        }
        let Self {
            window: _,
            conversion,
            line,
            drawing,
            shown,
            fresh,
            dot,
            row,
            reached,
            displaying,
            displayed,
        } = self;
        conversion.restore(input)?;
        line.restore(input)?;
        restore_bytes(drawing, input)?;
        restore_bytes(shown, input)?;
        fresh.restore(input)?;
        dot.restore(input)?;
        row.restore(input)?;
        reached.restore(input)?;
        displaying.restore(input)?;
        displayed.restore(input)?;
        check(*reached <= window.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Window = Window {
        left: 4,
        width: 8,
        top: 2,
        height: 3,
        hold_line: 0,
        free_line: 20,
        hold_frame: 0,
        free_frame: 10,
    };

    fn pixel(frame: super::super::Frame<'_>, x: usize, y: usize) -> [u8; 4] {
        let at = (y * 8 + x) * 4;
        frame.pixels[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn the_window_is_measured_from_the_sync_pulses() {
        let mut raster = Raster::new(WINDOW, Conversion::Rgb).unwrap();
        raster.vsync();
        for _ in 0..4 {
            raster.blank(2);
            raster.dots(&[0x00FF_0000; 6]);
            raster.dots(&[0x0000_00FF; 6]);
            raster.hsync();
        }
        raster.vsync();
        let frame = raster.take_frame().unwrap();
        assert_eq!(
            pixel(frame, 0, 0),
            [0xFF, 0, 0, 0xFF],
            "dot 4 after the third line's sync"
        );
        assert_eq!(pixel(frame, 4, 0), [0, 0, 0xFF, 0xFF]);
        assert_eq!(
            pixel(frame, 0, 2),
            [0, 0, 0, 0xFF],
            "the fifth line never came"
        );
        assert!(raster.take_frame().is_none(), "a frame is taken once");
    }

    #[test]
    fn a_frame_names_the_picture_inside_its_border() {
        let mut raster = Raster::new(WINDOW, Conversion::Rgb).unwrap();
        raster.vsync();
        for line in 0..6 {
            raster.blank(2);
            if (3..5).contains(&line) {
                raster.dots(&[0; 3]);
                raster.displayed(&[0x00FF_FFFF; 4]);
                raster.dots(&[0; 5]);
            } else {
                raster.dots(&[0; 12]);
            }
            raster.hsync();
        }
        raster.vsync();
        assert_eq!(
            raster.take_frame().unwrap().displayed,
            Some(Area {
                left: 1,
                top: 1,
                right: 5,
                bottom: 3,
            }),
            "lines 3 and 4 of the beam are the window's first two, dots 5 to 8 its 1 to 4"
        );
        for _ in 0..10 {
            raster.dots(&[0; 20]);
            raster.hsync();
        }
        raster.vsync();
        assert_eq!(
            raster.take_frame().unwrap().displayed,
            None,
            "a frame of border alone"
        );
    }

    #[test]
    fn a_missing_sync_lets_the_oscillators_run() {
        let mut raster = Raster::new(WINDOW, Conversion::Rgb).unwrap();
        raster.blank(20 * 10 - 1);
        assert!(raster.take_frame().is_none());
        raster.blank(1);
        assert!(
            raster.take_frame().is_some(),
            "ten free-running lines make a frame"
        );
    }

    #[test]
    fn syncs_too_soon_for_the_oscillators_are_ignored() {
        let held = Window {
            hold_line: 16,
            hold_frame: 8,
            ..WINDOW
        };
        let mut raster = Raster::new(held, Conversion::Rgb).unwrap();
        for _ in 0..8 {
            for _ in 0..5 {
                raster.blank(2);
                raster.hsync();
                raster.vsync();
            }
            raster.blank(6);
            raster.hsync();
        }
        assert!(
            raster.take_frame().is_none(),
            "eight lines of 16 dots, every pulse inside them ignored"
        );
        raster.vsync();
        assert!(
            raster.take_frame().is_some(),
            "the eighth line's end lets a vertical sync in"
        );
    }

    #[test]
    fn a_palette_converts_codes() {
        let palette = Palette::new(&[[0, 0, 0], [0xAA, 0x55, 0]]);
        let mut raster = Raster::new(WINDOW, Conversion::Palette(palette)).unwrap();
        raster.vsync();
        raster.hsync();
        raster.hsync();
        raster.blank(4);
        raster.dots(&[1; 8]);
        raster.hsync();
        raster.vsync();
        assert_eq!(
            pixel(raster.take_frame().unwrap(), 7, 0),
            [0xAA, 0x55, 0, 0xFF]
        );
    }
}
