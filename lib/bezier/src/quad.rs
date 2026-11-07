//! The quadratic Bézier segment.

use crate::cubic::CubicBez;
use crate::point::Point;

/// A quadratic Bézier from `p0` to `p2` drawn towards `p1`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct QuadBez {
    /// Where it starts.
    pub p0: Point,
    /// The control point.
    pub p1: Point,
    /// Where it ends.
    pub p2: Point,
}

impl QuadBez {
    /// The quadratic from `p0` to `p2` drawn towards `p1`.
    #[must_use]
    pub const fn new(p0: Point, p1: Point, p2: Point) -> Self {
        Self { p0, p1, p2 }
    }

    /// The point at parameter `t`.
    #[must_use]
    pub fn eval(self, t: f64) -> Point {
        let u = 1.0 - t;
        Point::new(
            u * u * self.p0.x + 2.0 * u * t * self.p1.x + t * t * self.p2.x,
            u * u * self.p0.y + 2.0 * u * t * self.p1.y + t * t * self.p2.y,
        )
    }

    /// The same curve as a cubic, exactly: degree elevation moves each
    /// control two thirds of the way from its end towards the quadratic's.
    #[must_use]
    pub fn raised(self) -> CubicBez {
        CubicBez::new(
            self.p0,
            self.p0 + (self.p1 - self.p0) * (2.0 / 3.0),
            self.p2 + (self.p1 - self.p2) * (2.0 / 3.0),
            self.p2,
        )
    }

    /// The direction it leaves `p0` in and the direction it arrives at `p2`
    /// in, falling back to the chord where the control sits on an end.
    #[must_use]
    pub fn tangents(self) -> (Point, Point) {
        (
            first_direction([self.p1 - self.p0, self.p2 - self.p0]),
            first_direction([self.p2 - self.p1, self.p2 - self.p0]),
        )
    }
}

/// The first candidate that states a direction, or no direction when every
/// one of them collapses.
pub(crate) fn first_direction<const N: usize>(candidates: [Point; N]) -> Point {
    candidates
        .into_iter()
        .find(|candidate| *candidate != Point::ZERO)
        .unwrap_or(Point::ZERO)
}

#[cfg(test)]
mod tests {
    use super::QuadBez;
    use crate::point::Point;

    #[test]
    fn raising_a_quadratic_keeps_every_point() {
        let quad = QuadBez::new(
            Point::new(0.0, 0.0),
            Point::new(4.0, 8.0),
            Point::new(10.0, 2.0),
        );
        let cubic = quad.raised();
        for step in 0..=10 {
            let t = f64::from(step) / 10.0;
            let (a, b) = (quad.eval(t), cubic.eval(t));
            assert!(a.distance(b) < 1e-12, "{a:?} vs {b:?} at {t}");
        }
    }

    #[test]
    fn a_control_on_an_end_falls_back_to_the_chord() {
        let quad = QuadBez::new(Point::ZERO, Point::ZERO, Point::new(3.0, 4.0));
        assert_eq!(quad.tangents().0, Point::new(3.0, 4.0));
    }
}
