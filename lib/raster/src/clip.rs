//! Clipping: what of a triangle survives a set of planes.
//!
//! One cut serves both spaces a mesh is clipped in — the view space a
//! [`Perspective`](crate::view::Perspective) sees, and the homogeneous clip
//! space a draw's vertex stage writes, mapped onto a [`Viewport`]. Each plane
//! cuts in turn, every edge from its inside end, so two triangles sharing an
//! edge cut it at the same point and a clipped mesh stays watertight. Each
//! corner carries its share of the triangle's own three, so whatever is
//! interpolated across the triangle is interpolated across what remains.
//!
//! The planes keep every surviving corner within a guard band a quarter of
//! the converter's extent about the screen's or viewport's centre, where the
//! converter's arithmetic stays exact. Everything is `f64` with the basic operations in a fixed order, so a
//! triangle clips to the same bits on every target.

use tairix_inline::ArrayVec;
use tairix_util::mathf;

use crate::mesh::MESH_SUB_UNITS;
use crate::scan::MAX_DRAWING_EXTENT;

/// How far from the centre a clipped corner may land, in pixels.
///
/// A quarter of the converter's extent, so with the centre anywhere a surface
/// can put it, every corner that survives the clip projects to where the
/// converter's arithmetic stays exact.
pub(crate) const GUARD_PIXELS: u32 = MAX_DRAWING_EXTENT / 4;

/// The most corners a triangle clipped to a [`Viewport`] has: each of the
/// seven planes it is cut by adds at most one.
pub const MAX_CLIP_CORNERS: usize = 10;

/// No corner is kept nearer the eye than this in `w`, whatever depth is
/// clamped to: past it the divide leaves nothing a pixel could show.
const LEAST_W: f64 = 1.0 / 1_073_741_824.0;

/// A corner while a clip is still cutting: where it is, and how much of each
/// of the triangle's own corners it is.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Cut<P> {
    pub(crate) at: P,
    pub(crate) weights: [f64; 3],
}

/// The planes of a space a triangle is clipped in.
pub(crate) trait Volume {
    /// A point of the space.
    type Point: Copy;
    /// A plane of the space.
    type Plane: Copy;

    /// How far inside `plane` `p` lies: inside where non-negative.
    fn distance(&self, plane: Self::Plane, p: &Self::Point) -> f64;

    /// `p`, a cut on `plane`, put exactly on it: the interpolated point lies
    /// on the plane only to within a rounding.
    fn onto(&self, plane: Self::Plane, p: Self::Point) -> Self::Point;

    /// The point `t` of the way from `a` to `b`.
    fn between(a: &Self::Point, b: &Self::Point, t: f64) -> Self::Point;
}

/// The value `t` of the way from `a` to `b`, component by component.
pub(crate) fn between<const N: usize>(a: &[f64; N], b: &[f64; N], t: f64) -> [f64; N] {
    core::array::from_fn(|axis| a[axis] + (b[axis] - a[axis]) * t)
}

/// Cut `triangle` by each of `planes` in turn, leaving in `kept` the convex
/// polygon that remains: `false` where nothing does, or where it would need
/// more than `N` corners.
pub(crate) fn cut<V: Volume, const N: usize>(
    volume: &V,
    planes: &[V::Plane],
    triangle: [V::Point; 3],
    kept: &mut ArrayVec<Cut<V::Point>, N>,
) -> bool {
    kept.clear();
    for (index, at) in triangle.into_iter().enumerate() {
        let mut weights = [0.0; 3];
        weights[index] = 1.0;
        if kept.try_push(Cut { at, weights }).is_err() {
            return false;
        }
    }
    let mut next = ArrayVec::<Cut<V::Point>, N>::new();
    for &plane in planes {
        if kept
            .iter()
            .all(|corner| volume.distance(plane, &corner.at) >= 0.0)
        {
            continue;
        }
        next.clear();
        for (index, &from) in kept.iter().enumerate() {
            let to = kept[(index + 1) % kept.len()];
            let (d_from, d_to) = (
                volume.distance(plane, &from.at),
                volume.distance(plane, &to.at),
            );
            let inside = d_from >= 0.0;
            if inside && next.try_push(from).is_err() {
                return false;
            }
            if inside != (d_to >= 0.0) {
                let (a, b, d_a, d_b) = if inside {
                    (from, to, d_from, d_to)
                } else {
                    (to, from, d_to, d_from)
                };
                let t = d_a / (d_a - d_b);
                let corner = Cut {
                    at: volume.onto(plane, V::between(&a.at, &b.at, t)),
                    weights: between(&a.weights, &b.weights, t),
                };
                if next.try_push(corner).is_err() {
                    return false;
                }
            }
        }
        core::mem::swap(kept, &mut next);
        if kept.len() < 3 {
            return false;
        }
    }
    true
}

/// A plane of homogeneous clip space.
#[derive(Copy, Clone, Debug)]
pub(crate) enum ClipPlane {
    /// `w` far enough from the eye to divide by.
    Positive,
    /// `z ≥ 0`.
    Near,
    /// `z ≤ w`.
    Far,
    /// `x ≥ −g·w`, the guard band's left edge.
    Left,
    /// `x ≤ g·w`.
    Right,
    /// `y ≥ −g·w`.
    Top,
    /// `y ≤ g·w`.
    Bottom,
}

/// Every plane, in the order each cuts.
const CLIPPED: [ClipPlane; 7] = [
    ClipPlane::Positive,
    ClipPlane::Near,
    ClipPlane::Far,
    ClipPlane::Left,
    ClipPlane::Right,
    ClipPlane::Top,
    ClipPlane::Bottom,
];

/// The planes left where depth is clamped rather than clipped.
const CLAMPED: [ClipPlane; 5] = [
    ClipPlane::Positive,
    ClipPlane::Left,
    ClipPlane::Right,
    ClipPlane::Top,
    ClipPlane::Bottom,
];

/// Where a draw's clip-space corners land: the rectangle they are mapped
/// onto — its height negative where the draw flips it — and the depths the
/// near and far planes map to.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Viewport {
    centre: (f64, f64),
    /// Half the width and half the height, signed as the draw stated them.
    half: (f64, f64),
    depth: (f64, f64),
    /// The guard band along each axis, in multiples of the viewport's half
    /// extent.
    guard: (f64, f64),
    clamp: bool,
}

/// A clipped corner where it lands.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Mapped {
    /// How much of each of the triangle's corners it is in clip space: what
    /// a perspective-correct value at it is made of.
    pub weights: [f64; 3],
    /// How much of each it is across the screen: what a value interpolated
    /// linearly in framebuffer space is made of.
    pub linear: [f64; 3],
    /// Across, in [`MESH_SUB_UNITS`] of a pixel.
    pub x: i32,
    /// Down, in sub-units.
    pub y: i32,
    /// Its depth on the viewport's range; with depth clamped it may lie past
    /// the range, for the depth stage to clamp.
    pub depth: f64,
    /// The reciprocal of its `w`: its perspective weight.
    pub inverse_w: f64,
}

impl Viewport {
    /// The viewport whose top-left is `origin` and whose size is `size`
    /// pixels, mapping depth onto `range` — `(near, far)`, either way round —
    /// and clamping depth rather than clipping it where `clamp` holds.
    ///
    /// `None` for a value that is not finite, a width that is not positive, a
    /// height of nought, a depth outside `0..=1`, or a rectangle so large or
    /// so far out that a corner kept within the guard band could leave the
    /// converter's exact range.
    #[must_use]
    pub fn new(
        origin: (f64, f64),
        size: (f64, f64),
        range: (f64, f64),
        clamp: bool,
    ) -> Option<Self> {
        let values = [origin.0, origin.1, size.0, size.1, range.0, range.1];
        if values.iter().any(|value| !value.is_finite()) {
            return None;
        }
        if size.0 <= 0.0 || size.1 == 0.0 {
            return None;
        }
        if [range.0, range.1]
            .iter()
            .any(|depth| !(0.0..=1.0).contains(depth))
        {
            return None;
        }
        let half = (size.0 * 0.5, size.1 * 0.5);
        let centre = (origin.0 + half.0, origin.1 + half.1);
        let guard = f64::from(GUARD_PIXELS);
        let reach = f64::from(MAX_DRAWING_EXTENT) - guard;
        let (wide, tall) = (mathf::fabs(half.0), mathf::fabs(half.1));
        if wide > guard
            || tall > guard
            || mathf::fabs(centre.0) > reach
            || mathf::fabs(centre.1) > reach
        {
            return None;
        }
        Some(Self {
            centre,
            half,
            depth: range,
            guard: (guard / wide, guard / tall),
            clamp,
        })
    }

    /// Clip `triangle`, clip-space positions `[x, y, z, w]`, and map what
    /// remains onto the viewport, leaving its corners in `out`: nothing where
    /// none survives, a corner is not finite, or the cut would need more than
    /// [`MAX_CLIP_CORNERS`] corners.
    pub fn clip(&self, triangle: [[f64; 4]; 3], out: &mut ArrayVec<Mapped, MAX_CLIP_CORNERS>) {
        out.clear();
        if triangle.iter().flatten().any(|value| !value.is_finite()) {
            return;
        }
        let planes: &[ClipPlane] = if self.clamp { &CLAMPED } else { &CLIPPED };
        let mut kept = ArrayVec::<Cut<[f64; 4]>, MAX_CLIP_CORNERS>::new();
        if !cut(self, planes, triangle, &mut kept) {
            return;
        }
        let w = triangle.map(|corner| corner[3]);
        for corner in &kept {
            let Some(mapped) = self.map(corner, w) else {
                out.clear();
                return;
            };
            // Room for every corner kept: both hold the same number.
            let _ = out.try_push(mapped);
        }
    }

    /// `corner`, one the clip kept of a triangle whose corners' `w` are `w`,
    /// on the viewport, or `None` where it would leave the converter's range.
    fn map(&self, corner: &Cut<[f64; 4]>, w: [f64; 3]) -> Option<Mapped> {
        let [x, y, z, own_w] = corner.at;
        let inverse_w = 1.0 / own_w;
        let screen = (
            self.centre.0 + self.half.0 * (x * inverse_w),
            self.centre.1 + self.half.1 * (y * inverse_w),
        );
        let extent = f64::from(MAX_DRAWING_EXTENT);
        if !(mathf::fabs(screen.0) <= extent && mathf::fabs(screen.1) <= extent) {
            return None;
        }
        let shares = core::array::from_fn(|index| corner.weights[index] * w[index]);
        let total = shares.iter().sum::<f64>();
        // Past the eye plane by a rounding, the shares' sum need not keep the
        // sign the cut gave the corner's own `w`.
        if !(total > 0.0 && total.is_finite()) {
            return None;
        }
        let sub = f64::from(MESH_SUB_UNITS);
        Some(Mapped {
            weights: corner.weights,
            linear: shares.map(|share: f64| share / total),
            x: mathf::round_i32(screen.0 * sub),
            y: mathf::round_i32(screen.1 * sub),
            depth: self.depth.0 + (self.depth.1 - self.depth.0) * (z * inverse_w),
            inverse_w,
        })
    }
}

impl Volume for Viewport {
    type Point = [f64; 4];
    type Plane = ClipPlane;

    fn distance(&self, plane: ClipPlane, at: &[f64; 4]) -> f64 {
        let [x, y, z, w] = *at;
        match plane {
            ClipPlane::Positive => w - LEAST_W,
            ClipPlane::Near => z,
            ClipPlane::Far => w - z,
            ClipPlane::Left => self.guard.0 * w + x,
            ClipPlane::Right => self.guard.0 * w - x,
            ClipPlane::Top => self.guard.1 * w + y,
            ClipPlane::Bottom => self.guard.1 * w - y,
        }
    }

    /// A cut onto a later plane is first held inside the earlier ones it can
    /// round back out of: `w` to the eye's, and depth to its range where it is
    /// clipped.
    fn onto(&self, plane: ClipPlane, mut p: [f64; 4]) -> [f64; 4] {
        if !matches!(plane, ClipPlane::Positive) {
            p[3] = mathf::fmax(p[3], LEAST_W);
        }
        if !self.clamp
            && !matches!(
                plane,
                ClipPlane::Positive | ClipPlane::Near | ClipPlane::Far
            )
        {
            p[2] = mathf::fmin(mathf::fmax(p[2], 0.0), p[3]);
        }
        match plane {
            ClipPlane::Positive => p[3] = LEAST_W,
            ClipPlane::Near => p[2] = 0.0,
            ClipPlane::Far => p[2] = p[3],
            ClipPlane::Left => p[0] = -(self.guard.0 * p[3]),
            ClipPlane::Right => p[0] = self.guard.0 * p[3],
            ClipPlane::Top => p[1] = -(self.guard.1 * p[3]),
            ClipPlane::Bottom => p[1] = self.guard.1 * p[3],
        }
        p
    }

    fn between(a: &[f64; 4], b: &[f64; 4], t: f64) -> [f64; 4] {
        between(a, b, t)
    }
}

#[cfg(test)]
#[path = "clip_tests.rs"]
mod tests;
