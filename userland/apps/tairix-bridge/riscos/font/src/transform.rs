//! Millipoints placed as RISC OS 5.30 places them: a font's thousandths of
//! an em scaled by its size, then carried through its matrix and the
//! matrix painting it, each product of a matrix and a coordinate floored on
//! its own, and positions kept in 32 bits that wrap.

use tairix_drawing::riscos::format::{FIXED_ONE, FRACTION_BITS};

use crate::identifier::Matrix;
use crate::metrics;
use crate::units;

/// One in a matrix's multipliers.
const ONE: i64 = FIXED_ONE as i64;

/// A matrix: four multipliers with `FRACTION_BITS` bits of fraction, and a
/// translation in millipoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Transform {
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    e: i64,
    f: i64,
}

impl Transform {
    pub(crate) const IDENTITY: Self = Self {
        a: ONE,
        b: 0,
        c: 0,
        d: ONE,
        e: 0,
        f: 0,
    };

    pub(crate) fn of(Matrix(words): Matrix) -> Self {
        let words = words.map(i64::from);
        Self {
            a: words[0],
            b: words[1],
            c: words[2],
            d: words[3],
            e: words[4],
            f: words[5],
        }
    }

    /// `self` and then `outer`: their multipliers combined, each floored,
    /// and `self`'s translation carried through `outer` in one floor.
    pub(crate) fn then(self, outer: Self) -> Self {
        let mul = |p: i64, q: i64, r: i64, s: i64| {
            let product =
                (i128::from(p) * i128::from(q) + i128::from(r) * i128::from(s)) >> FRACTION_BITS;
            i64::from(low32(product))
        };
        Self {
            a: mul(outer.a, self.a, outer.c, self.b),
            b: mul(outer.b, self.a, outer.d, self.b),
            c: mul(outer.a, self.c, outer.c, self.d),
            d: mul(outer.b, self.c, outer.d, self.d),
            e: i64::from(wrap(mul(outer.a, self.e, outer.c, self.f) + outer.e)),
            f: i64::from(wrap(mul(outer.b, self.e, outer.d, self.f) + outer.f)),
        }
    }

    /// The displacement `(x, y)` carried through the multipliers alone.
    pub(crate) fn vector(&self, (x, y): (i32, i32)) -> (i32, i32) {
        let (x, y) = (i64::from(x), i64::from(y));
        let across = ((self.a * x) >> FRACTION_BITS) + ((self.c * y) >> FRACTION_BITS);
        let up = ((self.b * x) >> FRACTION_BITS) + ((self.d * y) >> FRACTION_BITS);
        (wrap(across), wrap(up))
    }

    /// The point `(x, y)` carried through the matrix, its translation too.
    pub(crate) fn point(&self, at: (i32, i32)) -> (i32, i32) {
        let (x, y) = self.vector(at);
        (wrap(i64::from(x) + self.e), wrap(i64::from(y) + self.f))
    }

    /// The point `(x, y)`, in millipoints, carried through the matrix
    /// unrounded: where a glyph's outline is drawn.
    pub(crate) fn exact(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let [xx, xy, yx, yy, dx, dy] =
            [self.a, self.b, self.c, self.d, self.e, self.f].map(|v| f64::from(wrap(v)));
        let one = f64::from(FIXED_ONE);
        ((xx * x + yx * y) / one + dx, (xy * x + yy * y) / one + dy)
    }

    /// Whether its multipliers are the identity's: it moves, but neither
    /// scales, turns nor shears.
    pub(crate) const fn unscaled(&self) -> bool {
        self.a == ONE && self.b == 0 && self.c == 0 && self.d == ONE
    }

    /// How far the matrix stretches each axis, as RISC OS 5.30 holds a
    /// size to `FontMax`: the lengths of its rows, what across and what up
    /// on the screen draw from.
    pub(crate) fn scales(&self) -> (f64, f64) {
        let [a, b, c, d] = [self.a, self.b, self.c, self.d].map(|v| f64::from(wrap(v)));
        let one = f64::from(FIXED_ONE);
        let length = |x: f64, y: f64| tairix_util::mathf::sqrt(x * x + y * y) / one;
        (length(a, c), length(b, d))
    }

    /// The box holding `bbox`'s four corners carried through the matrix.
    pub(crate) fn bbox(&self, bbox: metrics::Box) -> metrics::Box {
        let corners = [
            self.point((bbox.x0, bbox.y0)),
            self.point((bbox.x0, bbox.y1)),
            self.point((bbox.x1, bbox.y0)),
            self.point((bbox.x1, bbox.y1)),
        ];
        let first = corners[0];
        corners.iter().fold(
            metrics::Box {
                x0: first.0,
                y0: first.1,
                x1: first.0,
                y1: first.1,
            },
            |b, &(x, y)| metrics::Box {
                x0: b.x0.min(x),
                y0: b.y0.min(y),
                x1: b.x1.max(x),
                y1: b.y1.max(y),
            },
        )
    }

    /// Its multipliers taking a point's units, `size` of the screen's each
    /// across and up, through it to the screen's `screen` each across and
    /// up, each rounded to a 65536th.
    pub(crate) fn multiplied(&self, size: (f64, f64), screen: (f64, f64)) -> [i64; 4] {
        let scaled = |value: i64, by: f64| {
            let product = tairix_util::mathf::round(f64::from(wrap(value)) * by);
            i64::from(crate::glyphs::saturated(product))
        };
        [
            scaled(self.a, size.0 * screen.0),
            scaled(self.b, size.0 * screen.1),
            scaled(self.c, size.1 * screen.0),
            scaled(self.d, size.1 * screen.1),
        ]
    }

    /// Its translation, in millipoints.
    pub(crate) fn translation(&self) -> (i64, i64) {
        (i64::from(wrap(self.e)), i64::from(wrap(self.f)))
    }

    /// Its six words, as a matrix block holds them.
    pub(crate) fn words(&self) -> [u32; 6] {
        [self.a, self.b, self.c, self.d, self.e, self.f].map(|v| wrap(v).cast_unsigned())
    }
}

/// `millems` thousandths of an em in millipoints at `sixteenths` of a
/// point, floored.
pub(crate) fn scaled(millems: i32, sixteenths: i32) -> i32 {
    wrap((i64::from(millems) * i64::from(sixteenths)).div_euclid(i64::from(units::SIXTEENTHS)))
}

/// `bbox` in thousandths of an em scaled to millipoints across and up.
pub(crate) fn scaled_box(bbox: metrics::Box, (across, up): (i32, i32)) -> metrics::Box {
    metrics::Box {
        x0: scaled(bbox.x0, across),
        y0: scaled(bbox.y0, up),
        x1: scaled(bbox.x1, across),
        y1: scaled(bbox.y1, up),
    }
}

/// `value`'s low 32 bits, as RISC OS's registers keep it.
pub(crate) fn wrap(value: i64) -> i32 {
    low32(i128::from(value))
}

const fn low32(value: i128) -> i32 {
    let bytes = value.to_le_bytes();
    i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests;
