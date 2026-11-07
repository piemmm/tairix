//! The 2×3 affine map vector geometry is placed by.
//!
//! [`Affine`] is SVG's `matrix(a b c d e f)`, in the same order and with the
//! same meaning, so a transform attribute becomes an [`Affine`] field for
//! field. A nested `transform` chain composes with [`Affine::then`], a
//! gradient carries the matrix into its own space, and a curve flattener
//! picks its tolerance from [`Affine::max_scale`].
//!
//! Every operation is total. Trigonometry comes from [`tairix_util::mathf`],
//! which answers a finite value for every finite input, and the operations
//! that can fail — [`Affine::invert`] on a matrix that collapses area,
//! [`Affine::decompose`] on the same — report it as `None` rather than handing
//! back a matrix full of infinities.

use core::f64::consts::PI;

use tairix_util::mathf;

use crate::point::Point;

/// A determinant at or below this magnitude is a transform that collapses
/// area, so it has no inverse to hand back.
///
/// Absolute rather than relative: the matrices here map artwork coordinates
/// to pixels, where a determinant this small already flattens the shape to
/// less than a millionth of a sub-pixel however it is scaled afterwards.
const DEGENERATE_DETERMINANT: f64 = 1e-12;

/// Radians per degree, the unit every angle below is authored in.
const RADIANS_PER_DEGREE: f64 = PI / 180.0;

/// An affine transform, laid out as SVG's `matrix(a b c d e f)`.
///
/// The mapping is `x' = a*x + c*y + e`, `y' = b*x + d*y + f`: `a`/`d` scale,
/// `b`/`c` shear and rotate, and `e`/`f` translate.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Affine {
    /// The `x'` coefficient of `x` — horizontal scale.
    pub a: f64,
    /// The `y'` coefficient of `x` — vertical shear.
    pub b: f64,
    /// The `x'` coefficient of `y` — horizontal shear.
    pub c: f64,
    /// The `y'` coefficient of `y` — vertical scale.
    pub d: f64,
    /// The horizontal translation.
    pub e: f64,
    /// The vertical translation.
    pub f: f64,
}

/// An affine map taken apart into the moves a designer names it by: scale
/// first, then a horizontal shear, then a rotation, then a translation
/// ([`Affine::decompose`], [`Affine::composed`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Decomposed {
    /// Where the origin lands.
    pub translate: Point,
    /// The turn, in degrees clockwise as drawn.
    pub rotation_degrees: f64,
    /// The horizontal shear, in degrees.
    pub skew_x_degrees: f64,
    /// The scale along each axis before the shear; a negative `y` is a flip.
    pub scale: Point,
}

impl Affine {
    /// The transform that leaves every point where it is.
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Move every point by `(tx, ty)`.
    #[must_use]
    pub const fn translate(tx: f64, ty: f64) -> Self {
        Self {
            e: tx,
            f: ty,
            ..Self::IDENTITY
        }
    }

    /// Scale about the origin by `sx` horizontally and `sy` vertically.
    #[must_use]
    pub const fn scale(sx: f64, sy: f64) -> Self {
        Self {
            a: sx,
            d: sy,
            ..Self::IDENTITY
        }
    }

    /// Rotate `deg` degrees clockwise about the origin — the direction SVG's
    /// `rotate(deg)` turns in a y-down coordinate system.
    #[must_use]
    pub fn rotate_degrees(deg: f64) -> Self {
        let radians = deg * RADIANS_PER_DEGREE;
        let (sin, cos) = (mathf::sin(radians), mathf::cos(radians));
        Self {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Rotate `deg` degrees about `(cx, cy)` — SVG's `rotate(deg cx cy)`.
    #[must_use]
    pub fn rotate_degrees_about(deg: f64, cx: f64, cy: f64) -> Self {
        Self::translate(-cx, -cy)
            .then(Self::rotate_degrees(deg))
            .then(Self::translate(cx, cy))
    }

    /// Shear horizontally by `deg` degrees — SVG's `skewX(deg)`.
    #[must_use]
    pub fn skew_x_degrees(deg: f64) -> Self {
        Self {
            c: mathf::tan(deg * RADIANS_PER_DEGREE),
            ..Self::IDENTITY
        }
    }

    /// Shear vertically by `deg` degrees — SVG's `skewY(deg)`.
    #[must_use]
    pub fn skew_y_degrees(deg: f64) -> Self {
        Self {
            b: mathf::tan(deg * RADIANS_PER_DEGREE),
            ..Self::IDENTITY
        }
    }

    /// Map `point` through this transform.
    #[must_use]
    pub fn apply(self, point: (f64, f64)) -> (f64, f64) {
        let (x, y) = point;
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Map `point` through this transform.
    #[must_use]
    pub fn map(self, point: Point) -> Point {
        Point::from(self.apply(point.tuple()))
    }

    /// Map the vector `v` through the linear part alone: a direction or a
    /// displacement, which no translation moves.
    #[must_use]
    pub fn map_vector(self, v: Point) -> Point {
        Point::new(self.a * v.x + self.c * v.y, self.b * v.x + self.d * v.y)
    }

    /// The transform that applies `self` first and `outer` second.
    ///
    /// Reading order matches SVG's nesting: an element's own `transform`
    /// composed `.then(parent)` places it inside its parent's space.
    #[must_use]
    pub fn then(self, outer: Self) -> Self {
        Self {
            a: outer.a * self.a + outer.c * self.b,
            b: outer.b * self.a + outer.d * self.b,
            c: outer.a * self.c + outer.c * self.d,
            d: outer.b * self.c + outer.d * self.d,
            e: outer.a * self.e + outer.c * self.f + outer.e,
            f: outer.b * self.e + outer.d * self.f + outer.f,
        }
    }

    /// The determinant of the linear part: the factor every area is scaled
    /// by, negative where the transform mirrors.
    #[must_use]
    pub fn determinant(self) -> f64 {
        self.a * self.d - self.b * self.c
    }

    /// The transform that undoes this one, or `None` when this one is
    /// degenerate.
    ///
    /// A transform whose determinant vanishes has flattened the plane onto a
    /// line or a point, and no matrix maps that back. The result is checked
    /// for finiteness too: a determinant just above the degeneracy threshold
    /// paired with huge coefficients would otherwise yield infinities, and a
    /// caller mapping a pixel back through those would get a `NaN` it cannot
    /// render.
    #[must_use]
    pub fn invert(self) -> Option<Self> {
        let det = self.determinant();
        if !det.is_finite() || mathf::fabs(det) <= DEGENERATE_DETERMINANT {
            return None;
        }
        let inverse = Self {
            a: self.d / det,
            b: -self.b / det,
            c: -self.c / det,
            d: self.a / det,
            e: (self.c * self.f - self.d * self.e) / det,
            f: (self.b * self.e - self.a * self.f) / det,
        };
        inverse.is_finite().then_some(inverse)
    }

    /// The largest factor by which this transform can stretch a length: the
    /// larger singular value of its linear part.
    ///
    /// This is the exact value, not a bound: a curve flattener divides its
    /// device-space tolerance by it, and a cheap over-estimate such as the
    /// larger row norm would subdivide a rotated curve about 40% more finely
    /// than the geometry needs. The closed form costs two square roots
    /// rather than an iteration.
    ///
    /// The answer is always finite and non-negative. A matrix carrying a
    /// non-finite coefficient has no measurable scale and answers `0.0` — the
    /// same answer a fully collapsed matrix gives, which a caller scaling a
    /// tolerance by it already has to handle.
    #[must_use]
    pub fn max_scale(self) -> f64 {
        // For M = [[a, c], [b, d]] the singular values satisfy
        // 2σ² = ‖M‖²_F ± sqrt(‖M‖⁴_F - 4·det²); the larger root is wanted.
        let frobenius = self.a * self.a + self.b * self.b + self.c * self.c + self.d * self.d;
        let det = self.determinant();
        let spread = mathf::sqrt(frobenius * frobenius - 4.0 * det * det);
        let largest = mathf::sqrt(f64::midpoint(frobenius, spread));
        if largest.is_finite() {
            largest
        } else {
            0.0
        }
    }

    /// Whether every coefficient is finite, so mapping a finite point through
    /// this transform yields a finite one.
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.a.is_finite()
            && self.b.is_finite()
            && self.c.is_finite()
            && self.d.is_finite()
            && self.e.is_finite()
            && self.f.is_finite()
    }

    /// The transform taken apart as a scale, a horizontal shear, a rotation
    /// and a translation, applied in that order, or `None` for one that
    /// collapses area or is not finite.
    ///
    /// The linear part is factored `R·K·S` (a QR factorisation): the
    /// rotation carries the image of the x axis back onto the axis, so the
    /// horizontal scale is that image's length and every mirror lands in the
    /// vertical scale's sign.
    #[must_use]
    pub fn decompose(self) -> Option<Decomposed> {
        let det = self.determinant();
        if !self.is_finite() || mathf::fabs(det) <= DEGENERATE_DETERMINANT {
            return None;
        }
        let scale_x = mathf::hypot(self.a, self.b);
        let scale_y = det / scale_x;
        let shear = (self.a * self.c + self.b * self.d) / scale_x;
        Some(Decomposed {
            translate: Point::new(self.e, self.f),
            rotation_degrees: mathf::atan2(self.b, self.a) / RADIANS_PER_DEGREE,
            skew_x_degrees: mathf::atan(shear / scale_y) / RADIANS_PER_DEGREE,
            scale: Point::new(scale_x, scale_y),
        })
    }

    /// The transform `parts` describes: [`Affine::decompose`]'s inverse.
    #[must_use]
    pub fn composed(parts: Decomposed) -> Self {
        Self::scale(parts.scale.x, parts.scale.y)
            .then(Self::skew_x_degrees(parts.skew_x_degrees))
            .then(Self::rotate_degrees(parts.rotation_degrees))
            .then(Self::translate(parts.translate.x, parts.translate.y))
    }
}

#[cfg(test)]
#[path = "affine_tests.rs"]
mod tests;
