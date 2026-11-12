//! The frame scaler: an opaque picture that changes every frame, enlarged into
//! a rectangle with its pixels kept square ("sharp bilinear").
//!
//! Each source pixel becomes a block of whole output pixels, and the fraction a
//! non-integer scale leaves is blended across the one output pixel at a block's
//! edge, so the picture is neither blurred, as a bilinear enlargement is, nor
//! uneven, as nearest-neighbour's blocks are. A shrink is plain bilinear.
//!
//! The asset resampler ([`resample`](fn@crate::resample)) is exact and runs
//! once per asset and size. This runs per frame, so its taps are worked out
//! once per pair of sizes and almost every output pixel is a copy.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_util::fallible;

use crate::color::Pixel;
use crate::resample::{Region, Rgba8Image};
use crate::surface::Surface;

/// Why a frame could not be drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharpError {
    /// The target rectangle has no area.
    EmptyTarget,
    /// A side of the frame or the target is longer than a scaler takes.
    TooLarge,
    /// The heap would not hold the scaler's tables.
    OutOfMemory,
}

/// One output pixel's taps on one axis: source pixel `index`, blended
/// `weight`/[`WEIGHT_ONE`] of the way to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tap {
    index: u32,
    weight: u16,
}

/// A tap's whole weight: all of the next source pixel.
const WEIGHT_ONE: u16 = 256;

/// Fixed-point unit of the tap arithmetic: one source pixel.
const ONE: i64 = 1 << 16;
const HALF: i64 = ONE / 2;

/// The longest axis a scaler takes, which keeps every tap's arithmetic
/// within `i64`.
const MAX_EXTENT: u32 = 1 << 16;

/// Output pixel `i`'s tap of `dest` across `source` pixels, the block of
/// whole pixels each source pixel makes `prescale` wide.
fn tap(i: u32, source: u32, dest: u32, prescale: u32) -> Tap {
    let centre = (2 * i64::from(i) + 1) * i64::from(source) * ONE / (2 * i64::from(dest));
    let (whole, within) = (centre / ONE, centre % ONE);
    let prescale = i64::from(prescale);
    // Inside a block the output pixel takes its source pixel whole; only the
    // last output pixel's width at the block's edge is blended.
    let reach = HALF - HALF / prescale;
    let from_middle = within - HALF;
    let sampled = whole * ONE + (from_middle - from_middle.clamp(-reach, reach)) * prescale;
    let last = i64::from(source) - 1;
    if sampled <= 0 {
        return Tap {
            index: 0,
            weight: 0,
        };
    }
    let index = sampled / ONE;
    if index >= last {
        return Tap {
            index: u32::try_from(last).unwrap_or(0),
            weight: 0,
        };
    }
    let weight = ((sampled % ONE) * i64::from(WEIGHT_ONE) + HALF) / ONE;
    Tap {
        index: u32::try_from(index).unwrap_or(0),
        weight: u16::try_from(weight).unwrap_or(WEIGHT_ONE),
    }
}

/// The taps of `dest` output pixels across `source` source pixels.
fn taps(source: u32, dest: u32) -> Option<Vec<Tap>> {
    let prescale = (dest / source).max(1);
    let count = usize::try_from(dest).ok()?;
    fallible::collected(count, (0..dest).map(|i| tap(i, source, dest, prescale)))
}

/// `a` taken `weight`/[`WEIGHT_ONE`] of the way to `b`, each channel rounded
/// to nearest. Written a channel at a time, which the compiler vectorises
/// across a row; a lane-packed form measured slower.
fn lerp(a: Pixel, b: Pixel, weight: u16) -> Pixel {
    let mix = |a: u8, b: u8| {
        let (w, a, b) = (u32::from(weight), u32::from(a), u32::from(b));
        u8::try_from((a * (u32::from(WEIGHT_ONE) - w) + b * w + 128) >> 8).unwrap_or(u8::MAX)
    };
    Pixel {
        r: mix(a.r, b.r),
        g: mix(a.g, b.g),
        b: mix(a.b, b.b),
        a: u8::MAX,
    }
}

/// A source pixel, made opaque: the picture is shown whole, whatever its
/// alpha says.
const fn opaque([r, g, b, _]: [u8; 4]) -> Pixel {
    Pixel {
        r,
        g,
        b,
        a: u8::MAX,
    }
}

/// One source row enlarged across the target's width.
#[derive(Debug, Default)]
struct Line {
    source_row: Option<u32>,
    pixels: Vec<Pixel>,
}

/// The sizes a scaler's taps were worked out for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sizes {
    source: (u32, u32),
    dest: (u32, u32),
}

/// A frame scaler, keeping its taps between frames of the same sizes.
#[derive(Debug, Default)]
pub struct SharpScaler {
    sizes: Option<Sizes>,
    columns: Vec<Tap>,
    rows: Vec<Tap>,
    lines: [Line; 2],
}

impl SharpScaler {
    /// A scaler with no sizes yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sizes: None,
            columns: Vec::new(),
            rows: Vec::new(),
            lines: [
                Line {
                    source_row: None,
                    pixels: Vec::new(),
                },
                Line {
                    source_row: None,
                    pixels: Vec::new(),
                },
            ],
        }
    }

    fn fit(&mut self, sizes: Sizes) -> Result<(), SharpError> {
        if self.sizes == Some(sizes) {
            return Ok(());
        }
        let ((source_w, source_h), (dest_w, dest_h)) = (sizes.source, sizes.dest);
        if [source_w, source_h, dest_w, dest_h]
            .iter()
            .any(|&extent| extent > MAX_EXTENT)
        {
            return Err(SharpError::TooLarge);
        }
        self.sizes = None;
        self.columns = taps(source_w, dest_w).ok_or(SharpError::OutOfMemory)?;
        self.rows = taps(source_h, dest_h).ok_or(SharpError::OutOfMemory)?;
        let width = usize::try_from(dest_w).map_err(|_| SharpError::OutOfMemory)?;
        for line in &mut self.lines {
            line.pixels =
                fallible::filled(width, Pixel::TRANSPARENT).ok_or(SharpError::OutOfMemory)?;
            line.source_row = None;
        }
        self.sizes = Some(sizes);
        Ok(())
    }

    /// Draw `rows` of `frame` enlarged to fill `target`, a rectangle of
    /// `surface`: the rows are the target's own, and those outside it are
    /// left. The surface's clip window applies.
    ///
    /// # Errors
    ///
    /// [`SharpError::EmptyTarget`] for a target with no area,
    /// [`SharpError::TooLarge`] for a side past 65,536 pixels, and
    /// [`SharpError::OutOfMemory`] when the heap will not hold the taps of a
    /// new pair of sizes.
    pub fn draw(
        &mut self,
        frame: &Rgba8Image<'_>,
        surface: &mut Surface,
        target: Region,
        rows: Range<u32>,
    ) -> Result<(), SharpError> {
        if target.width == 0 || target.height == 0 {
            return Err(SharpError::EmptyTarget);
        }
        self.fit(Sizes {
            source: (frame.width(), frame.height()),
            dest: (target.width, target.height),
        })?;
        for line in &mut self.lines {
            line.source_row = None;
        }
        for y in rows.start..rows.end.min(target.height) {
            let Some(&tap) = self.rows.get(y as usize) else {
                break;
            };
            let Some((first, span)) = target
                .y
                .checked_add(y)
                .and_then(|row| surface.row_span_mut(row, target.x, target.width))
            else {
                continue;
            };
            let skip = (first - target.x) as usize;
            let upper = self.line_of(frame, tap.index);
            if tap.weight == 0 {
                copy_from(span, &self.lines[upper].pixels, skip);
                continue;
            }
            let lower = self.line_of(frame, tap.index + 1);
            let (upper, lower) = (&self.lines[upper].pixels, &self.lines[lower].pixels);
            let pairs = upper.iter().zip(lower).skip(skip);
            for (out, (&a, &b)) in span.iter_mut().zip(pairs) {
                *out = lerp(a, b, tap.weight);
            }
        }
        Ok(())
    }

    /// Which of the two lines holds source row `row` enlarged, enlarging it
    /// into the one not holding the other row a target row may still want.
    fn line_of(&mut self, frame: &Rgba8Image<'_>, row: u32) -> usize {
        if let Some(held) = self.lines.iter().position(|l| l.source_row == Some(row)) {
            return held;
        }
        let into = match self.lines[0].source_row {
            Some(r) if r + 1 == row => 1,
            _ => 0,
        };
        let line = &mut self.lines[into];
        line.source_row = Some(row);
        let Some(source) = frame.row(row) else {
            line.pixels.fill(Pixel::TRANSPARENT);
            return into;
        };
        for (out, tap) in line.pixels.iter_mut().zip(&self.columns) {
            let at = tap.index as usize;
            let a = source.get(at).copied().map_or(Pixel::TRANSPARENT, opaque);
            *out = if tap.weight == 0 {
                a
            } else {
                let b = source.get(at + 1).copied().map_or(a, opaque);
                lerp(a, b, tap.weight)
            };
        }
        into
    }

    /// The target rows `source_rows` of the last frame's source reach: what a
    /// change to those rows makes it redraw.
    #[must_use]
    pub fn rows_reached(&self, source_rows: Range<u32>) -> Range<u32> {
        if source_rows.is_empty() {
            return 0..0;
        }
        let ends = |tap: &Tap| tap.index + u32::from(tap.weight != 0);
        let start = self
            .rows
            .partition_point(|tap| ends(tap) < source_rows.start);
        let end = self.rows.partition_point(|tap| tap.index < source_rows.end);
        let as_row = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        as_row(start)..as_row(end.max(start))
    }
}

/// `span` taking `line` from its column `skip` on.
fn copy_from(span: &mut [Pixel], line: &[Pixel], skip: usize) {
    if let Some(source) = line.get(skip..skip + span.len()) {
        span.copy_from_slice(source);
    }
}

#[cfg(test)]
#[path = "sharp_tests.rs"]
mod tests;
