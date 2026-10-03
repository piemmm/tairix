//! Rays, over the shared vectors, frames and poses the tracer is written in.

pub(crate) use tairix_util::space::{Frame, Pose, Vec3};

use crate::noise::cell;

/// A count as a real number: every count a scene holds lies far below where
/// a float stops counting exactly, and a larger one saturates.
pub(crate) fn real(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

/// The cell of a grid a non-negative `value` lies in, counted from nought,
/// and how far into it: nought for a negative or `NaN` one.
pub(crate) fn cell_of(value: f64) -> (usize, f64) {
    let (whole, fraction) = cell(value.max(0.0));
    (usize::try_from(whole).unwrap_or(usize::MAX), fraction)
}

/// `value` in single precision, rounded to the nearest; infinite past its
/// range.
#[allow(
    clippy::cast_possible_truncation,
    reason = "single precision is what each of the crate's grids, beams and parts keeps"
)]
pub(crate) fn single(value: f64) -> f32 {
    value as f32
}

/// Each of `value`'s three in single precision.
pub(crate) fn singles(value: Vec3) -> [f32; 3] {
    [single(value.x), single(value.y), single(value.z)]
}

/// `done` of `total` as a share in `0.0..=1.0`, whole when there was nothing
/// to do.
pub(crate) fn share(done: usize, total: usize) -> f64 {
    if total == 0 {
        1.0
    } else {
        (real(done) / real(total)).min(1.0)
    }
}

/// The most rays traced together: a sampling round's worth, every round a
/// whole number of them.
pub(crate) const PACKET: usize = 8;

const _: () = assert!(PACKET <= Members::LANES);

/// Some of a packet's rays, by their places in it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Members(u8);

impl Members {
    pub(crate) const NONE: Self = Self(0);

    /// The most rays a set can name.
    pub(crate) const LANES: usize = u8::BITS as usize;

    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(crate) const fn with(self, lane: usize) -> Self {
        Self(self.0 | 1 << lane)
    }

    pub(crate) const fn without(self, lane: usize) -> Self {
        Self(self.0 & !(1 << lane))
    }

    /// The first, if any.
    pub(crate) fn first(self) -> Option<usize> {
        (!self.is_empty()).then(|| self.0.trailing_zeros() as usize)
    }

    /// Each, first to last.
    pub(crate) fn lanes(self) -> impl Iterator<Item = usize> {
        let mut left = self.0;
        core::iter::from_fn(move || {
            let lane = (left != 0).then(|| left.trailing_zeros() as usize)?;
            left &= left - 1;
            Some(lane)
        })
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
