//! Rays, over the shared vectors, frames and poses the tracer is written in.

pub(crate) use tairix_util::space::{Frame, Pose, Vec3};

/// A count as a real number: every count a scene holds lies far below where
/// a float stops counting exactly, and a larger one saturates.
pub(crate) fn real(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
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

    /// This ray in the coordinates of a shape placed at `pose`.
    pub(crate) fn to_local(self, pose: &Pose) -> Self {
        Self::new(
            pose.point_to_local(self.origin),
            pose.frame.to_local(self.dir),
        )
    }
}

#[cfg(test)]
#[path = "vector_tests.rs"]
mod tests;
