//! The straight segment.

use crate::affine::Affine;
use crate::bounds::Bounds;
use crate::point::Point;

/// A straight segment from `p0` to `p1`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Line {
    /// Where it starts.
    pub p0: Point,
    /// Where it ends.
    pub p1: Point,
}

impl Line {
    /// The segment from `p0` to `p1`.
    #[must_use]
    pub const fn new(p0: Point, p1: Point) -> Self {
        Self { p0, p1 }
    }

    /// The point at parameter `t`.
    #[must_use]
    pub fn eval(self, t: f64) -> Point {
        self.p0.lerp(self.p1, t)
    }

    /// The constant derivative.
    #[must_use]
    pub fn deriv(self) -> Point {
        self.p1 - self.p0
    }

    /// The two halves either side of `t`.
    #[must_use]
    pub fn split(self, t: f64) -> (Self, Self) {
        let at = self.eval(t);
        (Self::new(self.p0, at), Self::new(at, self.p1))
    }

    /// The part from `t0` to `t1`.
    #[must_use]
    pub fn subsegment(self, t0: f64, t1: f64) -> Self {
        Self::new(self.eval(t0), self.eval(t1))
    }

    /// The bounds of both ends.
    #[must_use]
    pub const fn bounds(self) -> Bounds {
        Bounds::new(self.p0, self.p1)
    }

    /// The segment run the other way.
    #[must_use]
    pub const fn reversed(self) -> Self {
        Self::new(self.p1, self.p0)
    }

    /// The segment mapped through `transform`.
    #[must_use]
    pub fn transformed(self, transform: Affine) -> Self {
        Self::new(transform.map(self.p0), transform.map(self.p1))
    }

    /// Its length.
    #[must_use]
    pub fn length(self) -> f64 {
        self.p0.distance(self.p1)
    }

    /// The parameter of the point on the segment nearest `p`, and that
    /// point's squared distance from `p`.
    #[must_use]
    pub fn nearest(self, p: Point) -> (f64, f64) {
        let direction = self.deriv();
        let length_squared = direction.length_squared();
        let t = if length_squared > 0.0 {
            ((p - self.p0).dot(direction) / length_squared).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (t, self.eval(t).distance_squared(p))
    }

    /// This segment's share of the signed area of a closed path it belongs
    /// to: half the cross product of its ends, positive clockwise as drawn.
    #[must_use]
    pub fn signed_area(self) -> f64 {
        0.5 * self.p0.cross(self.p1)
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "every value compared is a small integer, exact by construction"
)]
mod tests {
    use super::Line;
    use crate::point::Point;

    #[test]
    fn nearest_is_held_to_the_segment() {
        let line = Line::new(Point::ZERO, Point::new(10.0, 0.0));
        assert_eq!(line.nearest(Point::new(3.0, 4.0)), (0.3, 16.0));
        assert_eq!(line.nearest(Point::new(-5.0, 0.0)), (0.0, 25.0));
        assert_eq!(line.nearest(Point::new(12.0, 0.0)), (1.0, 4.0));
        let dot = Line::new(Point::new(1.0, 1.0), Point::new(1.0, 1.0));
        assert_eq!(dot.nearest(Point::new(1.0, 2.0)), (0.0, 1.0));
    }

    #[test]
    fn a_closed_square_has_its_area() {
        let corners = [
            Point::ZERO,
            Point::new(2.0, 0.0),
            Point::new(2.0, 2.0),
            Point::new(0.0, 2.0),
        ];
        let area: f64 = (0..4)
            .map(|i| Line::new(corners[i], corners[(i + 1) % 4]).signed_area())
            .sum();
        assert_eq!(area, 4.0, "clockwise as drawn is positive");
    }
}
