//! Axis-aligned bounds of the plane.

use tairix_util::mathf::{fmax, fmin};

use crate::affine::Affine;
use crate::point::Point;

/// An axis-aligned rectangle of the plane, closed on every side, or empty.
///
/// Empty is any bounds whose minimum exceeds its maximum on either axis;
/// [`Bounds::EMPTY`] is the identity of [`Bounds::union`], so bounds are
/// gathered by folding from it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Bounds {
    /// The corner with the least coordinates.
    pub min: Point,
    /// The corner with the greatest coordinates.
    pub max: Point,
}

impl Default for Bounds {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Bounds {
    /// The bounds of nothing.
    pub const EMPTY: Self = Self {
        min: Point::new(f64::INFINITY, f64::INFINITY),
        max: Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
    };

    /// The bounds with corners `a` and `b`, in either order.
    #[must_use]
    pub const fn new(a: Point, b: Point) -> Self {
        Self {
            min: Point::new(fmin(a.x, b.x), fmin(a.y, b.y)),
            max: Point::new(fmax(a.x, b.x), fmax(a.y, b.y)),
        }
    }

    /// The bounds of the single point `p`.
    #[must_use]
    pub const fn of_point(p: Point) -> Self {
        Self { min: p, max: p }
    }

    /// The bounds of every point `points` yields.
    #[must_use]
    pub fn from_points(points: impl IntoIterator<Item = Point>) -> Self {
        points.into_iter().fold(Self::EMPTY, Self::union_point)
    }

    /// Whether the bounds hold no point.
    #[must_use]
    pub fn is_empty(self) -> bool {
        !(self.min.x <= self.max.x && self.min.y <= self.max.y)
    }

    /// The width, zero when empty.
    #[must_use]
    pub fn width(self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            self.max.x - self.min.x
        }
    }

    /// The height, zero when empty.
    #[must_use]
    pub fn height(self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            self.max.y - self.min.y
        }
    }

    /// The centre.
    #[must_use]
    pub fn centre(self) -> Point {
        self.min.midpoint(self.max)
    }

    /// The four corners, clockwise as drawn from the least.
    #[must_use]
    pub const fn corners(self) -> [Point; 4] {
        [
            self.min,
            Point::new(self.max.x, self.min.y),
            self.max,
            Point::new(self.min.x, self.max.y),
        ]
    }

    /// The bounds of both.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            min: Point::new(fmin(self.min.x, other.min.x), fmin(self.min.y, other.min.y)),
            max: Point::new(fmax(self.max.x, other.max.x), fmax(self.max.y, other.max.y)),
        }
    }

    /// The bounds of these and `p`.
    #[must_use]
    pub const fn union_point(self, p: Point) -> Self {
        self.union(Self::of_point(p))
    }

    /// What both hold, which may be empty.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self {
            min: Point::new(fmax(self.min.x, other.min.x), fmax(self.min.y, other.min.y)),
            max: Point::new(fmin(self.max.x, other.max.x), fmin(self.max.y, other.max.y)),
        }
    }

    /// Whether the two share a point, an edge or a corner included.
    #[must_use]
    pub fn intersects(self, other: Self) -> bool {
        !self.intersection(other).is_empty()
    }

    /// Whether `p` lies within, the edges included.
    #[must_use]
    pub fn contains(self, p: Point) -> bool {
        self.min.x <= p.x && p.x <= self.max.x && self.min.y <= p.y && p.y <= self.max.y
    }

    /// Whether every point of `other` lies within these.
    #[must_use]
    pub fn contains_bounds(self, other: Self) -> bool {
        other.is_empty()
            || (self.min.x <= other.min.x
                && other.max.x <= self.max.x
                && self.min.y <= other.min.y
                && other.max.y <= self.max.y)
    }

    /// The bounds grown by `margin` on every side; a negative margin shrinks
    /// them, to empty where it crosses.
    #[must_use]
    pub fn inflated(self, margin: f64) -> Self {
        if self.is_empty() {
            return self;
        }
        Self {
            min: Point::new(self.min.x - margin, self.min.y - margin),
            max: Point::new(self.max.x + margin, self.max.y + margin),
        }
    }

    /// The bounds of these corners mapped through `transform`, which hold
    /// the image of every point within.
    #[must_use]
    pub fn transformed(self, transform: Affine) -> Self {
        if self.is_empty() {
            return self;
        }
        Self::from_points(self.corners().map(|corner| transform.map(corner)))
    }

    /// Whether every coordinate is finite: false for empty bounds.
    #[must_use]
    pub const fn is_finite(self) -> bool {
        self.min.is_finite() && self.max.is_finite()
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "every value compared is a small integer, exact by construction"
)]
mod tests {
    use super::Bounds;
    use crate::affine::Affine;
    use crate::point::Point;

    #[test]
    fn empty_is_the_identity_of_union() {
        let b = Bounds::new(Point::new(1.0, 2.0), Point::new(-3.0, 4.0));
        assert_eq!(Bounds::EMPTY.union(b), b);
        assert_eq!(b.union(Bounds::EMPTY), b);
        assert!(Bounds::EMPTY.is_empty());
        assert_eq!(Bounds::EMPTY.width(), 0.0);
        assert!(!Bounds::EMPTY.is_finite());
    }

    #[test]
    fn corners_are_sorted_whichever_way_they_are_given() {
        let b = Bounds::new(Point::new(5.0, -1.0), Point::new(2.0, 3.0));
        assert_eq!(b.min, Point::new(2.0, -1.0));
        assert_eq!(b.max, Point::new(5.0, 3.0));
        assert_eq!(b.width(), 3.0);
        assert_eq!(b.height(), 4.0);
    }

    #[test]
    fn touching_bounds_intersect_and_disjoint_ones_do_not() {
        let a = Bounds::new(Point::ZERO, Point::new(1.0, 1.0));
        let touching = Bounds::new(Point::new(1.0, 0.0), Point::new(2.0, 1.0));
        let apart = Bounds::new(Point::new(1.5, 0.0), Point::new(2.0, 1.0));
        assert!(a.intersects(touching));
        assert!(!a.intersects(apart));
        assert!(a.intersection(apart).is_empty());
    }

    #[test]
    fn transformed_bounds_hold_every_rotated_corner() {
        let b = Bounds::new(Point::ZERO, Point::new(2.0, 1.0));
        let turned = b.transformed(Affine::rotate_degrees(90.0));
        for corner in b.corners() {
            let mapped = Affine::rotate_degrees(90.0).map(corner);
            assert!(turned.inflated(1e-9).contains(mapped));
        }
    }

    #[test]
    fn a_negative_margin_shrinks_to_empty() {
        let b = Bounds::new(Point::ZERO, Point::new(2.0, 2.0));
        assert!(b.inflated(-2.0).is_empty());
        assert!(!b.inflated(-0.5).is_empty());
        assert!(b.contains_bounds(b.inflated(-0.5)));
        assert!(b.contains_bounds(Bounds::EMPTY));
    }
}
