//! A machine's screen in its window: where the picture sits, which rows of
//! it a new frame changed, and the window's points as the frame's dots.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_raster::resample::{resample, Region};
use tairix_raster::{Rgba8Image, SharpError, SharpScaler, Surface};

use super::state::LastScreen;
use crate::link::layout::PIXEL;

/// A monitor's picture is four units wide for every three high, whatever its
/// card's dots and lines.
pub const ASPECT: (u32, u32) = (4, 3);

/// The largest picture of the monitor's shape a `width`×`height` window
/// holds, centred, on whole pixels; `None` for a window too small to hold
/// one.
#[must_use]
pub fn fit(width: u32, height: u32) -> Option<Region> {
    let (across, down) = ASPECT;
    let (w, h) = if u64::from(width) * u64::from(down) <= u64::from(height) * u64::from(across) {
        (width, width * down / across)
    } else {
        (height * across / down, height)
    };
    (w > 0 && h > 0).then(|| Region {
        x: (width - w) / 2,
        y: (height - h) / 2,
        width: w,
        height: h,
    })
}

/// The frame's dot and line under window point `(x, y)`, for a frame of
/// `frame` dots and lines drawn into `target`; a point outside the picture
/// is its nearest edge.
#[must_use]
pub fn frame_point(target: Region, frame: (u32, u32), (x, y): (i32, i32)) -> (u16, u16) {
    let along = |at: i32, from: u32, extent: u32, dots: u32| {
        let inside = i64::from(at) - i64::from(from);
        let clamped = inside.clamp(0, i64::from(extent.max(1)) - 1);
        let dot = clamped * i64::from(dots) / i64::from(extent.max(1));
        u16::try_from(dot).unwrap_or(u16::MAX)
    };
    (
        along(x, target.x, target.width, frame.0),
        along(y, target.y, target.height, frame.1),
    )
}

/// A `size` picture of a `width`×`height` frame of `pixels` as its monitor
/// shows it: the monitor's shape fitted and centred on black, the frame
/// resampled into it. None for pixels not the frame's, a size too small for
/// the shape, or a picture the heap will not hold.
#[must_use]
pub fn thumbnail(width: u32, height: u32, pixels: &[u8], size: (u32, u32)) -> Option<Vec<u8>> {
    let frame = Rgba8Image::new(width, height, pixels).ok()?;
    let at = fit(size.0, size.1)?;
    let whole = Region {
        x: 0,
        y: 0,
        width,
        height,
    };
    let picture = resample(&frame, whole, at.width, at.height).ok()?;
    let dots = |n: u32| usize::try_from(n).ok();
    let stride = dots(size.0)?.checked_mul(PIXEL)?;
    let len = stride.checked_mul(dots(size.1)?)?;
    let mut out = Vec::new();
    out.try_reserve_exact(len).ok()?;
    out.resize(len, 0);
    for pixel in out.as_chunks_mut::<PIXEL>().0 {
        pixel[PIXEL - 1] = 0xFF;
    }
    let row = dots(at.width)? * PIXEL;
    let left = dots(at.x)? * PIXEL;
    for (line, n) in picture.chunks_exact(row).zip(dots(at.y)?..) {
        let start = n * stride + left;
        out[start..start + row].copy_from_slice(line);
    }
    Some(out)
}

/// The narrowest a screenshot is: a VGA screen's width.
const SHOT_WIDTH: u32 = 640;

/// A `width`×`height` frame of `pixels` as its monitor showed it, for keeping
/// as a picture: four units wide for every three high, each of the card's
/// dots a block of whole pixels, at least as wide as a VGA screen. Its size, and
/// its straight RGBA; none for pixels not the frame's or a picture the heap
/// will not hold.
#[must_use]
pub fn screenshot(width: u32, height: u32, pixels: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let frame = Rgba8Image::new(width, height, pixels).ok()?;
    let wide = width.checked_mul(SHOT_WIDTH.div_ceil(width.max(1)))?;
    let (across, down) = ASPECT;
    let high = wide.checked_mul(down)? / across;
    let mut surface = Surface::new(wide, high)?;
    let target = Region {
        x: 0,
        y: 0,
        width: wide,
        height: high,
    };
    SharpScaler::new()
        .draw(&frame, &mut surface, target, 0..high)
        .ok()?;
    let mut out = Vec::new();
    out.try_reserve_exact(surface.pixels().len().checked_mul(PIXEL)?)
        .ok()?;
    // Every pixel is opaque, so its premultiplied channels are its straight
    // ones.
    for pixel in surface.pixels() {
        out.extend_from_slice(&[pixel.r, pixel.g, pixel.b, 0xFF]);
    }
    Some((wide, high, out))
}

/// The screen of one adapter: its last frame, the scaler that enlarges it,
/// and what of the window it is drawn into the next draw owes.
#[derive(Debug, Default)]
pub struct Screen {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    scaler: SharpScaler,
    /// Frame rows changed since the last draw.
    changed: Option<Range<u32>>,
    /// The target the last draw filled, while the frame is the size it was.
    drawn: Option<Region>,
}

impl Screen {
    /// A screen with no frame yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            pixels: Vec::new(),
            scaler: SharpScaler::new(),
            changed: None,
            drawn: None,
        }
    }

    /// The frame's dots and lines, `(0, 0)` before the first.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Take a `width`×`height` frame: whether it changed anything. Pixels
    /// that are not that frame, or a frame the heap will not hold, change
    /// nothing.
    pub fn take(&mut self, width: u32, height: u32, pixels: &[u8]) -> bool {
        let Some(stride) = usize::try_from(width).ok().map(|w| w * PIXEL) else {
            return false;
        };
        let whole = usize::try_from(height)
            .ok()
            .and_then(|h| h.checked_mul(stride));
        if stride == 0 || whole != Some(pixels.len()) {
            return false;
        }
        if (width, height) != (self.width, self.height) {
            let mut fresh = Vec::new();
            if fresh.try_reserve_exact(pixels.len()).is_err() {
                return false;
            }
            fresh.extend_from_slice(pixels);
            self.pixels = fresh;
            (self.width, self.height) = (width, height);
            self.drawn = None;
            return true;
        }
        let rows = self
            .pixels
            .chunks_exact(stride)
            .zip(pixels.chunks_exact(stride));
        let mut changed = rows.enumerate().filter(|(_, (old, new))| old != new);
        let Some((first, _)) = changed.next() else {
            return false;
        };
        let last = changed.next_back().map_or(first, |(row, _)| row);
        // The rows either side of the band are the same already.
        let band = first * stride..(last + 1) * stride;
        self.pixels[band.clone()].copy_from_slice(&pixels[band]);
        let as_row = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let (first, end) = (as_row(first), as_row(last + 1));
        self.changed = Some(match self.changed.take() {
            Some(was) => was.start.min(first)..was.end.max(end),
            None => first..end,
        });
        true
    }

    /// Redraw the whole picture at the next draw: what was drawn is gone.
    pub fn invalidate(&mut self) {
        self.drawn = None;
    }

    /// The frame's pixels: what its last screen is kept as.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The frame as a machine's last screen, to keep: none before the first
    /// frame, or where the heap will not hold a copy.
    #[must_use]
    pub fn last(&self) -> Option<LastScreen> {
        if self.width == 0 {
            return None;
        }
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(self.pixels.len()).ok()?;
        pixels.extend_from_slice(&self.pixels);
        Some(LastScreen {
            width: self.width,
            height: self.height,
            pixels,
        })
    }

    /// Draw into `target` what the frame changed since the last draw, or the
    /// whole picture into a target that is new: the target's rows drawn,
    /// none with no frame yet or nothing changed.
    ///
    /// # Errors
    ///
    /// As [`SharpScaler::draw`].
    pub fn draw(
        &mut self,
        surface: &mut Surface,
        target: Region,
    ) -> Result<Range<u32>, SharpError> {
        let Ok(frame) = Rgba8Image::new(self.width, self.height, &self.pixels) else {
            return Ok(0..0);
        };
        let rows = if self.drawn == Some(target) {
            match self.changed.take() {
                Some(changed) => self.scaler.rows_reached(changed),
                None => return Ok(0..0),
            }
        } else {
            self.changed = None;
            0..target.height
        };
        self.scaler.draw(&frame, surface, target, rows.clone())?;
        self.drawn = Some(target);
        Ok(rows)
    }
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
