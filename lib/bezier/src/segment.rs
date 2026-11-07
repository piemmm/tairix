//! A segment of a path: a line or a cubic.

use crate::affine::Affine;
use crate::bounds::Bounds;
use crate::cubic::CubicBez;
use crate::line::Line;
use crate::point::Point;

/// One segment of a path: a straight line or a cubic Bézier. A quadratic is
/// held as its exact cubic and an arc as the cubics that draw it, so every
/// algorithm over paths meets these two kinds alone.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Segment {
    /// A straight segment.
    Line(Line),
    /// A cubic Bézier.
    Cubic(CubicBez),
}

impl Segment {
    /// Where it starts.
    #[must_use]
    pub const fn start(self) -> Point {
        match self {
            Self::Line(line) => line.p0,
            Self::Cubic(cubic) => cubic.p0,
        }
    }

    /// Where it ends.
    #[must_use]
    pub const fn end(self) -> Point {
        match self {
            Self::Line(line) => line.p1,
            Self::Cubic(cubic) => cubic.p3,
        }
    }

    /// The point at parameter `t`.
    #[must_use]
    pub fn eval(self, t: f64) -> Point {
        match self {
            Self::Line(line) => line.eval(t),
            Self::Cubic(cubic) => cubic.eval(t),
        }
    }

    /// The derivative at parameter `t`.
    #[must_use]
    pub fn deriv(self, t: f64) -> Point {
        match self {
            Self::Line(line) => line.deriv(),
            Self::Cubic(cubic) => cubic.deriv(t),
        }
    }

    /// The two halves either side of `t`.
    #[must_use]
    pub fn split(self, t: f64) -> (Self, Self) {
        match self {
            Self::Line(line) => {
                let (a, b) = line.split(t);
                (Self::Line(a), Self::Line(b))
            }
            Self::Cubic(cubic) => {
                let (a, b) = cubic.split(t);
                (Self::Cubic(a), Self::Cubic(b))
            }
        }
    }

    /// The part from `t0` to `t1`, run backwards where `t1 < t0`.
    #[must_use]
    pub fn subsegment(self, t0: f64, t1: f64) -> Self {
        match self {
            Self::Line(line) => Self::Line(line.subsegment(t0, t1)),
            Self::Cubic(cubic) => Self::Cubic(cubic.subsegment(t0, t1)),
        }
    }

    /// The tight bounds of every point on it.
    #[must_use]
    pub fn bounds(self) -> Bounds {
        match self {
            Self::Line(line) => line.bounds(),
            Self::Cubic(cubic) => cubic.bounds(),
        }
    }

    /// The segment run the other way.
    #[must_use]
    pub const fn reversed(self) -> Self {
        match self {
            Self::Line(line) => Self::Line(line.reversed()),
            Self::Cubic(cubic) => Self::Cubic(cubic.reversed()),
        }
    }

    /// The segment mapped through `transform`.
    #[must_use]
    pub fn transformed(self, transform: Affine) -> Self {
        match self {
            Self::Line(line) => Self::Line(line.transformed(transform)),
            Self::Cubic(cubic) => Self::Cubic(cubic.transformed(transform)),
        }
    }

    /// The parameter of the point nearest `p`, and its squared distance.
    #[must_use]
    pub fn nearest(self, p: Point) -> (f64, f64) {
        match self {
            Self::Line(line) => line.nearest(p),
            Self::Cubic(cubic) => cubic.nearest(p),
        }
    }

    /// Its length, to within `accuracy`.
    #[must_use]
    pub fn arclen(self, accuracy: f64) -> f64 {
        match self {
            Self::Line(line) => line.length(),
            Self::Cubic(cubic) => cubic.arclen(accuracy),
        }
    }

    /// Its share of the signed area of a closed path, positive clockwise as
    /// drawn.
    #[must_use]
    pub fn signed_area(self) -> f64 {
        match self {
            Self::Line(line) => line.signed_area(),
            Self::Cubic(cubic) => cubic.signed_area(),
        }
    }

    /// The directions it leaves its start and arrives at its end in.
    #[must_use]
    pub fn tangents(self) -> (Point, Point) {
        match self {
            Self::Line(line) => (line.deriv(), line.deriv()),
            Self::Cubic(cubic) => cubic.tangents(),
        }
    }

    /// The segment as a cubic: a line becomes the cubic with its controls a
    /// third and two thirds along it, which traces the same points.
    #[must_use]
    pub fn as_cubic(self) -> CubicBez {
        match self {
            Self::Line(line) => CubicBez::new(
                line.p0,
                line.p0.lerp(line.p1, 1.0 / 3.0),
                line.p0.lerp(line.p1, 2.0 / 3.0),
                line.p1,
            ),
            Self::Cubic(cubic) => cubic,
        }
    }
}
