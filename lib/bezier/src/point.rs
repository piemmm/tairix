//! A point of the plane, or the vector between two.

use core::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

use tairix_util::mathf;

/// A point of the plane, or the vector between two points.
///
/// The y axis points down, as it does on a screen and in SVG, so a positive
/// [`Point::cross`] turns clockwise as drawn.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Point {
    /// The horizontal coordinate.
    pub x: f64,
    /// The vertical coordinate, growing downwards.
    pub y: f64,
}

impl Point {
    /// The origin, and the zero vector.
    pub const ZERO: Self = Self::new(0.0, 0.0);

    /// The point `(x, y)`.
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// The dot product with `other`.
    #[must_use]
    pub fn dot(self, other: Self) -> f64 {
        self.x * other.x + self.y * other.y
    }

    /// The z component of the cross product with `other`: positive where
    /// `other` lies clockwise of `self` as drawn.
    #[must_use]
    pub fn cross(self, other: Self) -> f64 {
        self.x * other.y - self.y * other.x
    }

    /// The vector's length.
    #[must_use]
    pub fn length(self) -> f64 {
        mathf::hypot(self.x, self.y)
    }

    /// The vector's length, squared.
    #[must_use]
    pub fn length_squared(self) -> f64 {
        self.dot(self)
    }

    /// The distance to `other`.
    #[must_use]
    pub fn distance(self, other: Self) -> f64 {
        (other - self).length()
    }

    /// The distance to `other`, squared.
    #[must_use]
    pub fn distance_squared(self, other: Self) -> f64 {
        (other - self).length_squared()
    }

    /// The point a fraction `t` of the way to `other`.
    #[must_use]
    pub fn lerp(self, other: Self, t: f64) -> Self {
        self + (other - self) * t
    }

    /// The point halfway to `other`.
    #[must_use]
    pub fn midpoint(self, other: Self) -> Self {
        Self::new(
            f64::midpoint(self.x, other.x),
            f64::midpoint(self.y, other.y),
        )
    }

    /// The vector of unit length along this one, or `None` for a vector too
    /// short or too long to have a direction.
    #[must_use]
    pub fn normalised(self) -> Option<Self> {
        let length = self.length();
        (length > 0.0 && length < f64::MAX).then(|| self / length)
    }

    /// The vector turned a quarter turn anticlockwise as drawn.
    #[must_use]
    pub const fn perpendicular(self) -> Self {
        Self::new(-self.y, self.x)
    }

    /// The vector's direction, in radians from the x axis towards the y axis.
    #[must_use]
    pub fn angle(self) -> f64 {
        mathf::atan2(self.y, self.x)
    }

    /// The unit vector at `radians` from the x axis towards the y axis.
    #[must_use]
    pub fn from_angle(radians: f64) -> Self {
        Self::new(mathf::cos(radians), mathf::sin(radians))
    }

    /// Whether both coordinates are finite.
    #[must_use]
    pub const fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }

    /// The point as an `(x, y)` pair.
    #[must_use]
    pub const fn tuple(self) -> (f64, f64) {
        (self.x, self.y)
    }
}

impl From<(f64, f64)> for Point {
    fn from((x, y): (f64, f64)) -> Self {
        Self::new(x, y)
    }
}

impl From<Point> for (f64, f64) {
    fn from(point: Point) -> Self {
        point.tuple()
    }
}

impl Add for Point {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }
}

impl AddAssign for Point {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl Sub for Point {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y)
    }
}

impl SubAssign for Point {
    fn sub_assign(&mut self, other: Self) {
        *self = *self - other;
    }
}

impl Mul<f64> for Point {
    type Output = Self;

    fn mul(self, factor: f64) -> Self {
        Self::new(self.x * factor, self.y * factor)
    }
}

impl Div<f64> for Point {
    type Output = Self;

    fn div(self, divisor: f64) -> Self {
        Self::new(self.x / divisor, self.y / divisor)
    }
}

impl Neg for Point {
    type Output = Self;

    fn neg(self) -> Self {
        Self::new(-self.x, -self.y)
    }
}

#[cfg(test)]
mod tests {
    use super::Point;

    #[test]
    fn arithmetic_is_componentwise() {
        let a = Point::new(1.0, 2.0);
        let b = Point::new(-3.0, 0.5);
        assert_eq!(a + b, Point::new(-2.0, 2.5));
        assert_eq!(a - b, Point::new(4.0, 1.5));
        assert_eq!(a * 2.0, Point::new(2.0, 4.0));
        assert_eq!(b / 0.5, Point::new(-6.0, 1.0));
        assert_eq!(-a, Point::new(-1.0, -2.0));
    }

    #[test]
    fn cross_is_positive_turning_clockwise_as_drawn() {
        let east = Point::new(1.0, 0.0);
        let south = Point::new(0.0, 1.0);
        assert!(east.cross(south) > 0.0, "y grows downwards");
        assert!(south.cross(east) < 0.0);
        assert_eq!(east.perpendicular(), Point::new(0.0, 1.0));
    }

    #[test]
    fn a_vector_with_no_length_has_no_direction() {
        assert_eq!(Point::ZERO.normalised(), None);
        assert_eq!(Point::new(f64::MAX, f64::MAX).normalised(), None);
        let unit = Point::new(3.0, 4.0).normalised();
        assert_eq!(unit, Some(Point::new(0.6, 0.8)));
    }

    #[test]
    fn lerp_and_midpoint_meet_halfway() {
        let a = Point::new(-2.0, 6.0);
        let b = Point::new(4.0, -2.0);
        assert_eq!(a.lerp(b, 0.5), a.midpoint(b));
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
    }
}
