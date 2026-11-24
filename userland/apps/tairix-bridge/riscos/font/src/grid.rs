//! The screen as RISC OS 5.30's Font Manager measures a paint on it, an axis
//! at a time: millipoints to pixels at the scale and the mode's eigen
//! factor, each glyph placed at the quarter of a pixel its pen is in and its
//! outline drawn in 512ths of a pixel of its font's own resolution.

use crate::outlines::PLACEMENTS;
use crate::units;

/// The largest eigen factor an axis is taken to have: past it a pixel
/// would outgrow the arithmetic, and no mode has one.
const MOST_EIG: u32 = 31;
/// Draw's units an OS unit, and the parts of a pixel an outline is drawn
/// in.
pub(crate) const DRAW_UNITS: i64 = 256;
const PARTS: i64 = 512;
/// The quarters of a pixel a glyph may be put at: the places a file holds
/// a character at.
#[expect(clippy::cast_possible_wrap, reason = "four places")]
pub(crate) const QUARTERS: i64 = PLACEMENTS as i64;

/// One axis of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Axis {
    /// Millipoints an OS unit, and a pixel.
    scale: i64,
    pixel: i64,
    eig: u32,
}

impl Axis {
    /// The axis `scale` millipoints an OS unit makes in a mode of eigen
    /// factor `eig`; none where the scale is not positive, on which RISC OS
    /// hangs.
    pub(crate) fn new(scale: i32, eig: u32) -> Option<Self> {
        let eig = eig.min(MOST_EIG);
        (scale > 0).then(|| Self {
            scale: i64::from(scale),
            pixel: i64::from(scale) << eig,
            eig,
        })
    }

    /// The dots an inch a pixel stands for: the resolution a font found or
    /// made at none of its own is drawn at.
    pub(crate) fn dots(self) -> i32 {
        clamped(((i64::from(units::MILLIPOINTS_AN_INCH) / self.scale) >> self.eig).max(1))
    }

    /// `units` OS units in millipoints, floored to the pixel they are in.
    pub(crate) fn os_units(self, units: i32) -> i64 {
        ((i64::from(units) >> self.eig) << self.eig) * self.scale
    }

    /// The quarter of a pixel the point `at` millipoints along is in.
    pub(crate) fn quarter(self, at: i64) -> i64 {
        narrowed((i128::from(at) * i128::from(QUARTERS)).div_euclid(i128::from(self.pixel)))
    }

    /// The pixel boundary nearest the point `at` millipoints along, halves
    /// up.
    pub(crate) fn nearest(self, at: i64) -> i64 {
        nearest(i128::from(at), i128::from(self.pixel))
    }

    /// The point `at` millipoints along, in pixels unrounded.
    pub(crate) fn exact(self, at: i64) -> f64 {
        exactly(at) / exactly(self.pixel)
    }

    /// Pixel `pixel`'s near edge, in millipoints.
    pub(crate) fn millipoints(self, pixel: i64) -> i64 {
        pixel.saturating_mul(self.pixel)
    }

    /// `millipoints` in 512ths of a pixel, the nearest, halves up: how far a
    /// matrix's translation moves an outline.
    pub(crate) fn parts(self, millipoints: i64) -> i64 {
        nearest(
            i128::from(millipoints) * i128::from(PARTS),
            i128::from(self.pixel),
        )
    }

    /// Quarter `quarter` of a pixel in 512ths of one.
    pub(crate) const fn quarter_parts(quarter: i64) -> i64 {
        quarter.saturating_mul(PARTS / QUARTERS)
    }

    /// `parts` 512ths of a pixel in Draw's units, halves up.
    pub(crate) fn draw_units(self, parts: i64) -> i32 {
        let units = (i128::from(parts) << self.eig) * i128::from(DRAW_UNITS);
        clamped(nearest(units, i128::from(PARTS)))
    }

    /// The pixel boundary `pixel` in Draw's units.
    pub(crate) fn pixel_draw_units(self, pixel: i64) -> i32 {
        clamped(narrowed(
            (i128::from(pixel) << self.eig) * i128::from(DRAW_UNITS),
        ))
    }

    /// `millipoints` in Draw's units, floored: an underline's edges.
    pub(crate) fn draw_units_of(self, millipoints: i64) -> i32 {
        clamped(narrowed(
            (i128::from(millipoints) * i128::from(DRAW_UNITS)).div_euclid(i128::from(self.scale)),
        ))
    }
}

/// The 512ths of a pixel a millipoint is in an outline drawn at `dots` an
/// inch, a pixel of it to a pixel of the screen.
pub(crate) fn parts_a_millipoint(dots: i32) -> f64 {
    f64::from(dots) * exactly(PARTS) / f64::from(units::MILLIPOINTS_AN_INCH)
}

/// How far across its pixel a glyph put at quarter `placement` of it is.
pub(crate) fn shift(placement: usize) -> f64 {
    exactly(i64::try_from(placement).unwrap_or(0)) / exactly(QUARTERS)
}

/// Both axes, across then up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Grid {
    pub(crate) across: Axis,
    pub(crate) up: Axis,
}

impl Grid {
    /// The grid `scale` millipoints an OS unit makes in a mode of eigen
    /// factors `eig`; none where either axis has no pixels.
    pub(crate) fn of(scale: (i32, i32), eig: (u32, u32)) -> Option<Self> {
        Some(Self {
            across: Axis::new(scale.0, eig.0)?,
            up: Axis::new(scale.1, eig.1)?,
        })
    }

    /// The point `at` in OS units, each floored to its pixel, in
    /// millipoints.
    pub(crate) fn os_units(self, (x, y): (i32, i32)) -> (i64, i64) {
        (self.across.os_units(x), self.up.os_units(y))
    }

    /// The quarter of a pixel each way the point `at` millipoints from the
    /// screen's origin is in.
    pub(crate) fn quarters(self, (x, y): (i64, i64)) -> (i64, i64) {
        (self.across.quarter(x), self.up.quarter(y))
    }

    /// The dots an inch a font found at `own` is drawn at: its own, a
    /// negative one its magnitude, or where it was found at none, the
    /// screen's.
    pub(crate) fn resolution(self, own: (i32, i32)) -> (i32, i32) {
        let dots = |own: i32, axis: Axis| match own.saturating_abs() {
            0 => axis.dots(),
            own => own,
        };
        (dots(own.0, self.across), dots(own.1, self.up))
    }
}

/// `value / divisor` to the nearest, halves up.
fn nearest(value: i128, divisor: i128) -> i64 {
    narrowed((2 * value + divisor).div_euclid(2 * divisor))
}

/// `value` held to what a paint's arithmetic can reach.
fn narrowed(value: i128) -> i64 {
    i64::try_from(value).unwrap_or(if value < 0 { i64::MIN } else { i64::MAX })
}

/// `value` held to 32 bits, the most a register or a Draw unit takes.
pub(crate) fn clamped(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

/// A whole number as a real one, exact within 2^53, which a paint's
/// millipoints are.
#[expect(
    clippy::cast_precision_loss,
    reason = "a paint's millipoints are within 2^53"
)]
pub(crate) const fn exactly(value: i64) -> f64 {
    value as f64
}

#[cfg(test)]
mod tests;
