//! A machine's screen beyond its window: the window's points as the frame's
//! dots, the library's thumbnail and the screenshot. The picture in the
//! window is `tairix_crt::screen`.

use alloc::vec::Vec;

use tairix_crt::screen::{fit, Screen, ASPECT};
use tairix_raster::resample::{resample, Region};
use tairix_raster::{Rgba8Image, SharpScaler, Surface};

use super::state::LastScreen;
use crate::link::layout::PIXEL;

/// A VGA screen's dots and lines, which a machine window opens to show at one
/// pixel a dot.
pub const VGA_SCREEN: (u32, u32) = (640, 480);

const _: () = assert!(VGA_SCREEN.0 * ASPECT.1 == VGA_SCREEN.1 * ASPECT.0);

/// The client a machine window opens at over a bar `bar` pixels tall: a VGA
/// screen at one pixel a dot, the picture [`fit`] places, above the bar.
#[must_use]
pub const fn opening_extent(bar: u32) -> (u32, u32) {
    (VGA_SCREEN.0, VGA_SCREEN.1.saturating_add(bar))
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
const SHOT_WIDTH: u32 = VGA_SCREEN.0;

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

/// The last frame `screen` showed, kept for when its machine is set aside;
/// `None` before its first, or when the heap will not hold the copy.
#[must_use]
pub fn last_screen(screen: &Screen) -> Option<LastScreen> {
    let (width, height) = screen.size();
    if width == 0 {
        return None;
    }
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(screen.pixels().len()).ok()?;
    pixels.extend_from_slice(screen.pixels());
    Some(LastScreen {
        width,
        height,
        pixels,
    })
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
