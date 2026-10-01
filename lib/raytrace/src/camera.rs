//! The camera: a thin lens focused at one distance, so what lies nearer or
//! farther softens as it would in a photograph.

use tairix_util::mathf;

use crate::vector::{Frame, Ray, Vec3};

#[derive(Copy, Clone, Debug)]
pub(crate) struct Camera {
    eye: Vec3,
    /// Right, up, and back out of the picture.
    frame: Frame,
    /// Half the view's height at unit distance.
    tan_half: f64,
    /// The picture's width over its height.
    aspect: f64,
    /// The lens's radius: `0.0` for a pinhole, sharp at every depth.
    aperture: f64,
    /// How far ahead the sharp plane is.
    focus: f64,
}

impl Camera {
    /// A camera at `eye` looking at `target` through a vertical field of view
    /// of `fov` radians onto a picture `aspect` times as wide as it is tall,
    /// its lens `aperture` in radius and focused `focus` ahead.
    pub(crate) fn looking(
        eye: Vec3,
        target: Vec3,
        fov: f64,
        aspect: f64,
        (aperture, focus): (f64, f64),
    ) -> Self {
        let forward = (target - eye).normalized();
        // Looking straight up or down, the vertical says nothing of which way
        // is right; the depth axis stands in for it.
        let level = forward.cross(Vec3::UP);
        let right = if level.length() > 1e-9 {
            level.normalized()
        } else {
            forward.cross(Vec3::new(0.0, 0.0, 1.0)).normalized()
        };
        let up = right.cross(forward);
        Self {
            eye,
            frame: Frame {
                x: right,
                y: up,
                z: -forward,
            },
            tan_half: mathf::tan(0.5 * fov),
            aspect,
            aperture,
            focus: focus.max(1e-3),
        }
    }

    /// Where the camera stands.
    pub(crate) const fn eye(&self) -> Vec3 {
        self.eye
    }

    /// The ray through film point `film`, each coordinate in `-1.0..=1.0`
    /// with x to the right and y up, passing through `lens`, a point of the
    /// unit disc.
    pub(crate) fn ray(&self, (x, y): (f64, f64), lens: (f64, f64)) -> Ray {
        let toward = self.frame.to_world(Vec3::new(
            x * self.tan_half * self.aspect,
            y * self.tan_half,
            -1.0,
        ));
        if self.aperture <= 0.0 {
            return Ray::new(self.eye, toward.normalized());
        }
        let sharp = self.eye + toward * self.focus;
        let origin = self.eye + (self.frame.x * lens.0 + self.frame.y * lens.1) * self.aperture;
        Ray::new(origin, (sharp - origin).normalized())
    }

    /// The angle one pixel spans at the centre of a picture `height` pixels
    /// tall: how wide a footprint a ray samples at unit distance.
    pub(crate) fn pixel_angle(&self, height: u32) -> f64 {
        2.0 * self.tan_half / f64::from(height.max(1))
    }

    /// Where `point` falls on the film, each coordinate in `-1.0..=1.0` when
    /// it is in the picture; `None` behind the camera.
    #[cfg(test)]
    pub(crate) fn project(&self, point: Vec3) -> Option<(f64, f64)> {
        let local = self.frame.to_local(point - self.eye);
        let depth = -local.z;
        (depth > 1e-9).then(|| {
            (
                local.x / (depth * self.tan_half * self.aspect),
                local.y / (depth * self.tan_half),
            )
        })
    }
}

#[cfg(test)]
#[path = "camera_tests.rs"]
mod tests;
