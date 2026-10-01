//! The slab test, written once over a lane type: one value at a time for a
//! single box, or four boxes a lane each, in arrays a vector unit takes whole
//! — SSE2 or NEON two lanes an instruction, a target with no vector unit one
//! — and lane for lane the same operations, so the same bits either way.

use core::ops::{Add, Mul, Sub};

use tairix_util::mathf::{fmax, fmin};

use crate::vector::Vec3;

/// A number the slab test runs over: one value, or a lane each of four.
pub(crate) trait Lane:
    Copy + Add<Output = Self> + Sub<Output = Self> + Mul<Output = Self>
{
    fn splat(value: f64) -> Self;
    fn magnitude(self) -> Self;
    /// The greater, by one compare and select: the operands are never `NaN`.
    fn greater(self, other: Self) -> Self;
    /// The lesser, by one compare and select: the operands are never `NaN`.
    fn lesser(self, other: Self) -> Self;
}

impl Lane for f64 {
    #[inline]
    fn splat(value: f64) -> Self {
        value
    }

    #[inline]
    fn magnitude(self) -> Self {
        self.abs()
    }

    #[inline]
    fn greater(self, other: Self) -> Self {
        fmax(self, other)
    }

    #[inline]
    fn lesser(self, other: Self) -> Self {
        fmin(self, other)
    }
}

/// Four values, one to a box. Aligned so a target that will not load a
/// vector from an unaligned address still loads one whole.
#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(C, align(32))]
pub(crate) struct Lanes(pub(crate) [f64; 4]);

impl Lanes {
    #[inline]
    fn map(self, each: impl Fn(f64) -> f64) -> Self {
        Self(self.0.map(each))
    }

    #[inline]
    fn zip(self, other: Self, each: impl Fn(f64, f64) -> f64) -> Self {
        Self(core::array::from_fn(|lane| {
            each(self.0[lane], other.0[lane])
        }))
    }
}

impl Add for Lanes {
    type Output = Self;

    #[inline]
    fn add(self, other: Self) -> Self {
        self.zip(other, |a, b| a + b)
    }
}

impl Sub for Lanes {
    type Output = Self;

    #[inline]
    fn sub(self, other: Self) -> Self {
        self.zip(other, |a, b| a - b)
    }
}

impl Mul for Lanes {
    type Output = Self;

    #[inline]
    fn mul(self, other: Self) -> Self {
        self.zip(other, |a, b| a * b)
    }
}

impl Lane for Lanes {
    #[inline]
    fn splat(value: f64) -> Self {
        Self([value; 4])
    }

    #[inline]
    fn magnitude(self) -> Self {
        self.map(f64::abs)
    }

    #[inline]
    fn greater(self, other: Self) -> Self {
        self.zip(other, fmax)
    }

    #[inline]
    fn lesser(self, other: Self) -> Self {
        self.zip(other, fmin)
    }
}

/// A box's low and high corners, each coordinate a lane: one box, or four.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Corners<L> {
    pub(crate) min: [L; 3],
    pub(crate) max: [L; 3],
}

impl<L: Lane> Corners<L> {
    /// Grown by a sliver of its own size, so a ray grazing a face exactly,
    /// or one rounding nudges outside it, is still let in.
    #[inline]
    pub(crate) fn padded(self) -> Self {
        let [lx, ly, lz] = self.min.map(Lane::magnitude);
        let [hx, hy, hz] = self.max.map(Lane::magnitude);
        let size = lx
            .greater(ly)
            .greater(lz)
            .greater(hx.greater(hy).greater(hz));
        let pad = L::splat(1e-9) * (L::splat(1.0) + size);
        Self {
            min: self.min.map(|low| low - pad),
            max: self.max.map(|high| high + pad),
        }
    }

    /// Where the ray from `origin` with reciprocal direction `inverse` enters
    /// the box, no nearer than its origin, and leaves it, however far: it
    /// crosses the box only if the first is no further than the second. The
    /// box must be finite, since the test takes no care over a `NaN` one
    /// could make; a lane holding one answers nonsense, for its caller to
    /// ignore.
    #[inline]
    pub(crate) fn crossing(&self, origin: Vec3, inverse: Vec3) -> (L, L) {
        let slab = |axis: usize| {
            let (from, over) = (L::splat(origin.along(axis)), L::splat(inverse.along(axis)));
            let (a, b) = (
                (self.min[axis] - from) * over,
                (self.max[axis] - from) * over,
            );
            (a.lesser(b), a.greater(b))
        };
        let ((x0, x1), (y0, y1), (z0, z1)) = (slab(0), slab(1), slab(2));
        (
            x0.greater(y0).greater(z0).greater(L::splat(0.0)),
            x1.lesser(y1).lesser(z1),
        )
    }
}

#[cfg(test)]
#[path = "lanes_tests.rs"]
mod tests;
