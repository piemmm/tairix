//! Points, directions, orthonormal frames and rigid poses in three
//! dimensions.

use core::ops::{Add, AddAssign, Div, Mul, Neg, Sub};

use crate::mathf;

/// A point, a direction, or a triple combined component by component, such
/// as a linear RGB radiance.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Vec3 {
    /// The first component.
    pub x: f64,
    /// The second component: up, where a direction has one.
    pub y: f64,
    /// The third component.
    pub z: f64,
}

impl Vec3 {
    /// Every component zero.
    pub const ZERO: Self = Self::splat(0.0);
    /// Every component one.
    pub const ONE: Self = Self::splat(1.0);
    /// The unit vertical.
    pub const UP: Self = Self::new(0.0, 1.0, 0.0);

    /// The vector with these components.
    #[must_use]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    /// The vector with every component `value`.
    #[must_use]
    pub const fn splat(value: f64) -> Self {
        Self::new(value, value, value)
    }

    /// The dot product.
    #[must_use]
    pub fn dot(self, other: Self) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    /// The right-handed cross product.
    #[must_use]
    pub fn cross(self, other: Self) -> Self {
        Self::new(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }

    /// The Euclidean length.
    #[must_use]
    pub fn length(self) -> f64 {
        mathf::sqrt(self.dot(self))
    }

    /// This direction at unit length; the zero vector stays zero.
    #[must_use]
    pub fn normalized(self) -> Self {
        let length = self.length();
        if length > 0.0 {
            self * (1.0 / length)
        } else {
            self
        }
    }

    /// `self` mirrored about the unit `normal`.
    #[must_use]
    pub fn reflect(self, normal: Self) -> Self {
        self - normal * (2.0 * self.dot(normal))
    }

    /// The lesser of each component.
    #[must_use]
    pub fn min(self, other: Self) -> Self {
        Self::new(
            self.x.min(other.x),
            self.y.min(other.y),
            self.z.min(other.z),
        )
    }

    /// The greater of each component.
    #[must_use]
    pub fn max(self, other: Self) -> Self {
        Self::new(
            self.x.max(other.x),
            self.y.max(other.y),
            self.z.max(other.z),
        )
    }

    /// The greatest component.
    #[must_use]
    pub fn max_element(self) -> f64 {
        self.x.max(self.y).max(self.z)
    }

    /// How bright this triple looks as a linear Rec. 709 colour.
    #[must_use]
    pub fn luminance(self) -> f64 {
        0.2126 * self.x + 0.7152 * self.y + 0.0722 * self.z
    }

    /// `self` moved `t` of the way to `to`.
    #[must_use]
    pub fn lerp(self, to: Self, t: f64) -> Self {
        self + (to - self) * t
    }

    /// `e` raised to each component: the transmittance of an absorption
    /// optical depth `-self`.
    #[must_use]
    pub fn exp(self) -> Self {
        Self::new(mathf::exp(self.x), mathf::exp(self.y), mathf::exp(self.z))
    }

    /// Whether every component is finite.
    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    /// The component along `axis`: `0` for x, `1` for y, anything else z.
    #[must_use]
    pub fn along(self, axis: usize) -> f64 {
        match axis {
            0 => self.x,
            1 => self.y,
            _ => self.z,
        }
    }
}

impl Add for Vec3 {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }
}

impl AddAssign for Vec3 {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl Sub for Vec3 {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }
}

impl Neg for Vec3 {
    type Output = Self;

    fn neg(self) -> Self {
        Self::new(-self.x, -self.y, -self.z)
    }
}

impl Mul<f64> for Vec3 {
    type Output = Self;

    fn mul(self, scale: f64) -> Self {
        Self::new(self.x * scale, self.y * scale, self.z * scale)
    }
}

/// Component-wise, as a radiance is filtered by a reflectance.
impl Mul for Vec3 {
    type Output = Self;

    fn mul(self, other: Self) -> Self {
        Self::new(self.x * other.x, self.y * other.y, self.z * other.z)
    }
}

impl Div<f64> for Vec3 {
    type Output = Self;

    fn div(self, divisor: f64) -> Self {
        self * (1.0 / divisor)
    }
}

/// An orthonormal basis: three unit axes at right angles.
///
/// Read as a rotation, its axes are where it carries the world's own.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Frame {
    /// Where the world's x axis is carried.
    pub x: Vec3,
    /// Where the world's y axis is carried.
    pub y: Vec3,
    /// Where the world's z axis is carried.
    pub z: Vec3,
}

impl Frame {
    /// The world's own basis: no rotation at all.
    pub const WORLD: Self = Self {
        x: Vec3::new(1.0, 0.0, 0.0),
        y: Vec3::UP,
        z: Vec3::new(0.0, 0.0, 1.0),
    };

    /// A basis whose `z` is the unit `normal`, the other two chosen without a
    /// branch on its direction (Duff et al., "Building an Orthonormal Basis,
    /// Revisited", JCGT 2017).
    #[must_use]
    pub fn around(normal: Vec3) -> Self {
        let sign = if normal.z >= 0.0 { 1.0 } else { -1.0 };
        let a = -1.0 / (sign + normal.z);
        let b = normal.x * normal.y * a;
        Self {
            x: Vec3::new(
                1.0 + sign * normal.x * normal.x * a,
                sign * b,
                -sign * normal.x,
            ),
            y: Vec3::new(b, sign + normal.y * normal.y * a, -normal.y),
            z: normal,
        }
    }

    /// The world basis turned `yaw` radians about the vertical, then `tilt`
    /// radians about its own `x`: a positive tilt lowers `z`.
    #[must_use]
    pub fn turned(yaw: f64, tilt: f64) -> Self {
        let (sy, cy) = (mathf::sin(yaw), mathf::cos(yaw));
        let (st, ct) = (mathf::sin(tilt), mathf::cos(tilt));
        let x = Vec3::new(cy, 0.0, -sy);
        let level = Vec3::new(sy, 0.0, cy);
        Self {
            x,
            y: Vec3::UP * ct + level * st,
            z: level * ct - Vec3::UP * st,
        }
    }

    /// The rotation of `angle` radians about the unit `axis`, right-handed
    /// (Rodrigues' formula).
    #[must_use]
    pub fn about(axis: Vec3, angle: f64) -> Self {
        let (sin, cos) = (mathf::sin(angle), mathf::cos(angle));
        let turn = |v: Vec3| v * cos + axis.cross(v) * sin + axis * (axis.dot(v) * (1.0 - cos));
        Self {
            x: turn(Self::WORLD.x),
            y: turn(Self::WORLD.y),
            z: turn(Self::WORLD.z),
        }
    }

    /// The rotation carrying the direction `from` onto the direction `to`,
    /// composed with this basis: orthonormal, whatever rounding a basis
    /// turned many times over has gathered.
    #[must_use]
    pub fn aligning(self, from: Vec3, to: Vec3) -> Self {
        let (from, to) = (from.normalized(), to.normalized());
        let cos = from.dot(to);
        if cos < -0.5 {
            // Toward a half turn the axis the two directions span is lost in
            // rounding, so turn half way round a right angle to `from` first
            // and align what is left, which is then less than a right angle.
            let side = Self::around(from).x;
            let half = |v: Vec3| side * (2.0 * side.dot(v)) - v;
            let turned = Self {
                x: half(self.x),
                y: half(self.y),
                z: half(self.z),
            };
            return turned.aligning(-from, to);
        }
        let axis = from.cross(to);
        // Rodrigues' rotation, with the angle folded into `axis` and `cos`.
        let turn = |v: Vec3| v * cos + axis.cross(v) + axis * (axis.dot(v) / (1.0 + cos));
        let y = turn(self.y).normalized();
        let x = turn(self.x);
        let x = (x - y * x.dot(y)).normalized();
        Self {
            x,
            y,
            z: x.cross(y),
        }
    }

    /// This basis turned by `turn`: each axis carried by that rotation.
    #[must_use]
    pub fn rotated_by(self, turn: Self) -> Self {
        Self {
            x: turn.to_world(self.x),
            y: turn.to_world(self.y),
            z: turn.to_world(self.z),
        }
    }

    /// A vector given in this basis's coordinates, in world coordinates.
    #[must_use]
    pub fn to_world(self, local: Vec3) -> Vec3 {
        self.x * local.x + self.y * local.y + self.z * local.z
    }

    /// A world vector in this basis's coordinates.
    #[must_use]
    pub fn to_local(self, world: Vec3) -> Vec3 {
        Vec3::new(world.dot(self.x), world.dot(self.y), world.dot(self.z))
    }
}

/// Where a shape stands and how it is turned: a rigid placement.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Pose {
    /// Where the shape's own origin stands.
    pub at: Vec3,
    /// How the shape's own axes are turned.
    pub frame: Frame,
}

impl Pose {
    /// The shape's own origin at `at`, its axes turned to `frame`.
    #[must_use]
    pub const fn new(at: Vec3, frame: Frame) -> Self {
        Self { at, frame }
    }

    /// A world point in the shape's own coordinates.
    #[must_use]
    pub fn point_to_local(&self, world: Vec3) -> Vec3 {
        self.frame.to_local(world - self.at)
    }

    /// A point in the shape's own coordinates, in the world's.
    #[must_use]
    pub fn point_to_world(&self, local: Vec3) -> Vec3 {
        self.at + self.frame.to_world(local)
    }
}

#[cfg(test)]
#[path = "space_tests.rs"]
mod tests;
