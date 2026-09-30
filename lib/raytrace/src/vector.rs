//! Vectors, rays and rigid frames: the arithmetic the tracer is written in.

use core::ops::{Add, AddAssign, Div, Mul, Neg, Sub};

use tairix_util::mathf;

/// A count as a real number: every count a scene holds lies far below where
/// a float stops counting exactly, and a larger one saturates.
pub(crate) fn real(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

/// A point, a direction, or a linear RGB radiance.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct Vec3 {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) z: f64,
}

impl Vec3 {
    pub(crate) const ZERO: Self = Self::splat(0.0);
    pub(crate) const ONE: Self = Self::splat(1.0);
    pub(crate) const UP: Self = Self::new(0.0, 1.0, 0.0);

    pub(crate) const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub(crate) const fn splat(value: f64) -> Self {
        Self::new(value, value, value)
    }

    pub(crate) fn dot(self, other: Self) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    pub(crate) fn cross(self, other: Self) -> Self {
        Self::new(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }

    pub(crate) fn length(self) -> f64 {
        mathf::sqrt(self.dot(self))
    }

    /// This direction at unit length; the zero vector stays zero.
    pub(crate) fn normalized(self) -> Self {
        let length = self.length();
        if length > 0.0 {
            self * (1.0 / length)
        } else {
            self
        }
    }

    /// `self` mirrored about the unit `normal`.
    pub(crate) fn reflect(self, normal: Self) -> Self {
        self - normal * (2.0 * self.dot(normal))
    }

    pub(crate) fn min(self, other: Self) -> Self {
        Self::new(
            self.x.min(other.x),
            self.y.min(other.y),
            self.z.min(other.z),
        )
    }

    pub(crate) fn max(self, other: Self) -> Self {
        Self::new(
            self.x.max(other.x),
            self.y.max(other.y),
            self.z.max(other.z),
        )
    }

    pub(crate) fn max_element(self) -> f64 {
        self.x.max(self.y).max(self.z)
    }

    pub(crate) fn lerp(self, to: Self, t: f64) -> Self {
        self + (to - self) * t
    }

    /// `e` raised to each component: the transmittance of an absorption
    /// optical depth `-self`.
    pub(crate) fn exp(self) -> Self {
        Self::new(mathf::exp(self.x), mathf::exp(self.y), mathf::exp(self.z))
    }

    pub(crate) fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    /// The component along `axis`: `0` for x, `1` for y, anything else z.
    pub(crate) fn along(self, axis: usize) -> f64 {
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

/// A half-line: where it starts, and its unit direction.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Ray {
    pub(crate) origin: Vec3,
    pub(crate) dir: Vec3,
}

impl Ray {
    pub(crate) const fn new(origin: Vec3, dir: Vec3) -> Self {
        Self { origin, dir }
    }

    pub(crate) fn at(&self, t: f64) -> Vec3 {
        self.origin + self.dir * t
    }
}

/// An orthonormal basis: three unit axes at right angles.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Frame {
    pub(crate) x: Vec3,
    pub(crate) y: Vec3,
    pub(crate) z: Vec3,
}

impl Frame {
    pub(crate) const WORLD: Self = Self {
        x: Vec3::new(1.0, 0.0, 0.0),
        y: Vec3::UP,
        z: Vec3::new(0.0, 0.0, 1.0),
    };

    /// A basis whose `z` is the unit `normal`, the other two chosen without a
    /// branch on its direction (Duff et al., "Building an Orthonormal Basis,
    /// Revisited", JCGT 2017).
    pub(crate) fn around(normal: Vec3) -> Self {
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
    /// radians about its own `x`.
    pub(crate) fn turned(yaw: f64, tilt: f64) -> Self {
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

    /// The rotation carrying the unit `from` onto the unit `to`, composed
    /// with this basis.
    pub(crate) fn aligning(self, from: Vec3, to: Vec3) -> Self {
        let axis = from.cross(to);
        let cos = from.dot(to);
        let turn = |v: Vec3| -> Vec3 {
            if cos < -1.0 + 1e-12 {
                // Antipodal: a half turn about any axis at right angles to `from`.
                let side = Self::around(from).x;
                return side * (2.0 * side.dot(v)) - v;
            }
            // Rodrigues' rotation, with the angle folded into `axis` and `cos`.
            v * cos + axis.cross(v) + axis * (axis.dot(v) / (1.0 + cos))
        };
        Self {
            x: turn(self.x),
            y: turn(self.y),
            z: turn(self.z),
        }
    }

    /// This basis turned by `turn`: each axis carried by that rotation.
    pub(crate) fn rotated_by(self, turn: Self) -> Self {
        Self {
            x: turn.to_world(self.x),
            y: turn.to_world(self.y),
            z: turn.to_world(self.z),
        }
    }

    /// A vector given in this basis's coordinates, in world coordinates.
    pub(crate) fn to_world(self, local: Vec3) -> Vec3 {
        self.x * local.x + self.y * local.y + self.z * local.z
    }

    /// A world vector in this basis's coordinates.
    pub(crate) fn to_local(self, world: Vec3) -> Vec3 {
        Vec3::new(world.dot(self.x), world.dot(self.y), world.dot(self.z))
    }
}

/// Where a shape stands and how it is turned: a rigid placement.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Pose {
    pub(crate) at: Vec3,
    pub(crate) frame: Frame,
}

impl Pose {
    pub(crate) const fn new(at: Vec3, frame: Frame) -> Self {
        Self { at, frame }
    }

    pub(crate) fn point_to_local(&self, world: Vec3) -> Vec3 {
        self.frame.to_local(world - self.at)
    }

    pub(crate) fn ray_to_local(&self, ray: &Ray) -> Ray {
        Ray::new(
            self.point_to_local(ray.origin),
            self.frame.to_local(ray.dir),
        )
    }
}

#[cfg(test)]
#[path = "vector_tests.rs"]
mod tests;
